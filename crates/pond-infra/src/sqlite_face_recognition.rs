//! SQLite-backed [`FaceRecognition`]: ONNX embeddings stored as little-endian f32 BLOBs.
//!
//! Plain max-cosine lets noisy enrollments and collapsed embedders through, hence the extra
//! gates below (top-K/centroid pooling, S-norm, runner-up margin, open-set gap, min samples).

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::user_data::domain::face_recognition::{
    BoundingBox, DetectedFace, FaceEmbedding, FaceIdentification, FaceLandmarks,
};
use pond_core::user_data::ports::face_detector::FaceDetector;
use pond_core::user_data::ports::face_embedding_extractor::FaceEmbeddingExtractor;
use pond_core::user_data::ports::face_recognition::{FaceRecognition, PairwiseSimilarity};
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// ArcFace-512 cosine threshold; 0.60 admitted too many near neighbours.
const DEFAULT_MATCH_THRESHOLD: f32 = 0.70;

/// Added to the threshold when one profile is enrolled: the cross-profile gates are then no-ops.
const DEFAULT_SINGLE_PROFILE_MARGIN: f32 = 0.10;

/// Minimum face size in source-image pixels; smaller detections embed unreliably.
const MIN_DETECTED_FACE_PX: u32 = 80;

/// Top-K pooling of per-profile similarities.
const TOP_K: usize = 3;

/// Per-profile score is `w * centroid + (1-w) * topk_mean`; the centroid is steadier.
const CENTROID_WEIGHT: f32 = 0.6;

/// S-norm: rank by `raw - w * mean(other raws)`. 0.5 removes half a shared "everyone matches"
/// baseline without over-penalising true matches in a small cohort.
const SNORM_WEIGHT: f32 = 0.5;

/// The best profile must beat the runner-up by this much; tuned on ArcFace aligned crops.
const DEFAULT_RUNNER_UP_MARGIN: f32 = 0.10;

/// Profiles with fewer enrollments are never matched; 1 so single-enrollment ponds work.
const DEFAULT_MIN_SAMPLES_TO_IDENTIFY: usize = 1;

/// The winner's normalised score must beat the mean of all others by this much (open-set).
const DEFAULT_OPEN_SET_GAP_TO_MEAN_MIN: f32 = 0.08;

/// Max eye-line tilt; beyond it embeddings degrade even with alignment.
const MAX_EYE_TILT_RAD: f32 = 0.436; // ≈ 25°

/// Slack, as a fraction of inter-eye distance, for the nose to sit between the eyes' x.
const NOSE_CENTERING_SLACK: f32 = 0.35;

pub struct SqliteFaceRecognition {
    pool: Pool<Sqlite>,
    extractor: Arc<dyn FaceEmbeddingExtractor>,
    detector: Option<Arc<dyn FaceDetector>>,
    model_name: String,
    threshold: f32,
    runner_up_margin: f32,
    open_set_gap_min: f32,
    min_samples_to_identify: usize,
    single_profile_margin: f32,
    /// An operator-set threshold is used verbatim, without the single-profile margin:
    /// compressed-cosine ONNX exports need ~0.995+ whatever the profile count.
    threshold_is_user_set: bool,
}

/// A numeric env var clamped to `lo..=hi`, or `default` when unset or unparseable.
fn env_f32(name: &str, default: f32, lo: f32, hi: f32) -> f32 {
    match std::env::var(name).ok().and_then(|s| s.parse::<f32>().ok()) {
        Some(v) if v.is_finite() => v.clamp(lo, hi),
        _ => default,
    }
}

fn env_usize(name: &str, default: usize, lo: usize, hi: usize) -> usize {
    match std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    {
        Some(v) => v.clamp(lo, hi),
        _ => default,
    }
}

impl SqliteFaceRecognition {
    pub fn new(pool: Pool<Sqlite>, extractor: Arc<dyn FaceEmbeddingExtractor>) -> Self {
        Self {
            pool,
            extractor,
            detector: None,
            model_name: "arcface".to_string(),
            threshold: env_f32(
                "POND_FACE_MATCH_THRESHOLD",
                DEFAULT_MATCH_THRESHOLD,
                0.0,
                1.0,
            ),
            runner_up_margin: env_f32(
                "POND_FACE_RUNNER_UP_MARGIN",
                DEFAULT_RUNNER_UP_MARGIN,
                0.0,
                1.0,
            ),
            open_set_gap_min: env_f32(
                "POND_FACE_OPEN_SET_GAP",
                DEFAULT_OPEN_SET_GAP_TO_MEAN_MIN,
                0.0,
                1.0,
            ),
            min_samples_to_identify: env_usize(
                "POND_FACE_MIN_SAMPLES",
                DEFAULT_MIN_SAMPLES_TO_IDENTIFY,
                1,
                16,
            ),
            single_profile_margin: env_f32(
                "POND_FACE_SINGLE_PROFILE_MARGIN",
                DEFAULT_SINGLE_PROFILE_MARGIN,
                0.0,
                0.3,
            ),
            threshold_is_user_set: std::env::var("POND_FACE_MATCH_THRESHOLD")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .is_some(),
        }
    }

    pub fn with_model_name(mut self, name: impl Into<String>) -> Self {
        self.model_name = name.into();
        self
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self.threshold_is_user_set = true;
        self
    }

    /// With landmarks the extractor aligns by similarity transform, else it crops by bbox.
    pub fn with_detector(mut self, detector: Arc<dyn FaceDetector>) -> Self {
        self.detector = Some(detector);
        self
    }

    async fn lookup_profile_threshold(&self, profile_id: &str) -> Result<Option<f32>> {
        let row: Option<(Option<f64>,)> = sqlx::query_as(
            "SELECT match_threshold FROM face_profile_thresholds WHERE profile_id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&self.pool)
        .await
        .context("face_profile_thresholds lookup failed")?;
        Ok(row.and_then(|(t,)| t).map(|t| t as f32))
    }

    /// `None` removes the override (global threshold applies); `t` is clamped to \[0.0, 1.0\].
    pub async fn set_profile_threshold(
        &self,
        profile_id: &str,
        threshold: Option<f32>,
        note: Option<&str>,
    ) -> Result<()> {
        match threshold {
            None => {
                sqlx::query("DELETE FROM face_profile_thresholds WHERE profile_id = ?")
                    .bind(profile_id)
                    .execute(&self.pool)
                    .await
                    .context("failed to clear per-profile face threshold")?;
            }
            Some(raw) => {
                let clamped = raw.clamp(0.0, 1.0) as f64;
                sqlx::query(
                    "INSERT INTO face_profile_thresholds \
                       (profile_id, match_threshold, note, updated_at) \
                     VALUES (?, ?, ?, datetime('now')) \
                     ON CONFLICT(profile_id) DO UPDATE SET \
                       match_threshold = excluded.match_threshold, \
                       note            = excluded.note, \
                       updated_at      = datetime('now')",
                )
                .bind(profile_id)
                .bind(clamped)
                .bind(note)
                .execute(&self.pool)
                .await
                .context("failed to upsert per-profile face threshold")?;
            }
        }
        Ok(())
    }

    /// The per-profile threshold override; `None` means the global one applies.
    pub async fn get_profile_threshold(&self, profile_id: &str) -> Result<Option<f32>> {
        self.lookup_profile_threshold(profile_id).await
    }

    /// `Ok(None)` means no detector attached; no face or a failed quality gate is an `Err`.
    async fn run_detector(&self, image_bytes: &[u8]) -> Result<Option<DetectedFace>> {
        let Some(det) = self.detector.as_ref() else {
            return Ok(None);
        };
        let found = det
            .detect_face(image_bytes)
            .await
            .context("face detector failed")?;
        match found {
            None => Err(anyhow!("no face detected in image")),
            Some(face) => {
                if face.bbox.width < MIN_DETECTED_FACE_PX || face.bbox.height < MIN_DETECTED_FACE_PX
                {
                    return Err(anyhow!(
                        "detected face too small ({}×{} px); move closer to the camera",
                        face.bbox.width,
                        face.bbox.height
                    ));
                }
                if let Some(lms) = face.landmarks.as_ref() {
                    if let Err(e) = check_pose_sane(lms) {
                        return Err(e);
                    }
                }
                Ok(Some(face))
            }
        }
    }
}

/// Rejects extreme rotation or profile shots, which even alignment cannot rescue.
fn check_pose_sane(lms: &FaceLandmarks) -> Result<()> {
    let (lex, ley) = lms.left_eye;
    let (rex, rey) = lms.right_eye;
    let (nx, _ny) = lms.nose;
    let dx = rex - lex;
    let dy = rey - ley;
    let inter_eye = (dx * dx + dy * dy).sqrt();
    if inter_eye < 1e-3 {
        return Err(anyhow!("degenerate eye landmarks"));
    }
    // Eye-line tilt (radians off horizontal).  atan2 is robust to sign.
    let tilt = dy.atan2(dx).abs();
    let tilt_norm = if tilt > std::f32::consts::FRAC_PI_2 {
        std::f32::consts::PI - tilt
    } else {
        tilt
    };
    if tilt_norm > MAX_EYE_TILT_RAD {
        return Err(anyhow!(
            "face too tilted ({:.1}°); hold head level",
            tilt_norm.to_degrees()
        ));
    }
    // Nose centering, as a fraction of inter-eye distance outside the eyes' x span.
    let eye_lo = lex.min(rex);
    let eye_hi = lex.max(rex);
    let slack = NOSE_CENTERING_SLACK * (eye_hi - eye_lo).abs().max(1.0);
    if nx < eye_lo - slack || nx > eye_hi + slack {
        return Err(anyhow!("face not frontal enough; turn toward the camera"));
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct FaceRow {
    id: String,
    profile_id: String,
    embedding: Vec<u8>,
    model_dims: i64,
    created_at: String,
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| Utc::now())
}

fn pack_embedding(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn unpack_embedding(bytes: &[u8]) -> Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return Err(anyhow!(
            "embedding BLOB length {} not a multiple of 4",
            bytes.len()
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn row_to_embedding(row: FaceRow) -> Result<FaceEmbedding> {
    Ok(FaceEmbedding {
        id: row.id,
        profile_id: row.profile_id,
        embedding: unpack_embedding(&row.embedding)?,
        model_dims: row.model_dims as u32,
        created_at: parse_dt(&row.created_at),
    })
}

/// Unit-length mean of `embeddings`; `None` if empty or the mean has zero norm.
fn centroid_unit(embeddings: &[&Vec<f32>]) -> Option<Vec<f32>> {
    let first = embeddings.first()?;
    let dims = first.len();
    if dims == 0 {
        return None;
    }
    let mut acc = vec![0.0_f32; dims];
    for e in embeddings {
        if e.len() != dims {
            return None;
        }
        for i in 0..dims {
            acc[i] += e[i];
        }
    }
    let norm: f32 = acc.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm <= 1e-8 {
        return None;
    }
    for v in acc.iter_mut() {
        *v /= norm;
    }
    Some(acc)
}

/// Cosine similarity in \[-1.0, 1.0\].  Returns 0.0 for zero-norm vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[async_trait]
impl FaceRecognition for SqliteFaceRecognition {
    async fn register_face(
        &self,
        profile_id: &str,
        image_bytes: &[u8],
        bbox_override: Option<BoundingBox>,
    ) -> Result<FaceEmbedding> {
        // Prefer detector output; fall back to caller-supplied bbox.
        let (bbox, landmarks): (Option<BoundingBox>, Option<FaceLandmarks>) =
            match self.run_detector(image_bytes).await {
                Ok(Some(face)) => (Some(face.bbox), face.landmarks),
                Ok(None) => (bbox_override, None),
                Err(e) => {
                    // A caller's explicit bbox wins even if the detector disagrees.
                    if bbox_override.is_some() {
                        (bbox_override, None)
                    } else {
                        return Err(e);
                    }
                }
            };

        let embedding = self
            .extractor
            .extract_embedding(image_bytes, bbox, landmarks)
            .await
            .context("face embedding extraction failed")?
            .ok_or_else(|| {
                anyhow!(
                    "face failed quality gate during enrollment — check server log for the \
                 specific gate (low-variance / extreme-brightness / blurry / anti-spoof). \
                 Common knobs: POND_FACE_ANTISPOOF=off, POND_FACE_ANTISPOOF_THRESHOLD=0.85"
                )
            })?;

        let dims = self.extractor.embedding_dims();
        if embedding.len() != dims as usize {
            return Err(anyhow!(
                "extractor returned {} dims but advertises {}",
                embedding.len(),
                dims
            ));
        }

        let id = Uuid::new_v4().to_string();
        let packed = pack_embedding(&embedding);
        sqlx::query(
            "INSERT INTO face_embeddings \
             (id, profile_id, embedding, model_dims, model_name, created_at) \
             VALUES (?, ?, ?, ?, ?, datetime('now'))",
        )
        .bind(&id)
        .bind(profile_id)
        .bind(&packed)
        .bind(dims as i64)
        .bind(&self.model_name)
        .execute(&self.pool)
        .await
        .context("failed to insert face embedding")?;

        info!(%profile_id, %id, dims, aligned = landmarks.is_some(), "face embedding enrolled");

        Ok(FaceEmbedding {
            id,
            profile_id: profile_id.to_string(),
            embedding,
            model_dims: dims,
            created_at: Utc::now(),
        })
    }

    async fn identify_face(
        &self,
        image_bytes: &[u8],
        bbox_override: Option<BoundingBox>,
    ) -> Result<FaceIdentification> {
        Ok(self
            .identify_with_diagnostics(image_bytes, bbox_override)
            .await?
            .identification)
    }

    async fn identify_with_diagnostics(
        &self,
        image_bytes: &[u8],
        bbox_override: Option<BoundingBox>,
    ) -> Result<pond_core::user_data::ports::face_recognition::FaceIdentificationDetails> {
        use pond_core::user_data::ports::face_recognition::FaceIdentificationDetails;

        let (bbox, landmarks) = match self.run_detector(image_bytes).await {
            Ok(Some(face)) => (Some(face.bbox), face.landmarks),
            Ok(None) => (bbox_override, None),
            Err(e) => {
                debug!("identify_face: detector rejected frame: {e}");
                return Ok(FaceIdentificationDetails {
                    identification: FaceIdentification::no_face(),
                    landmarks: None,
                    embedding: None,
                    bbox: None,
                });
            }
        };

        let query_embedding = match self
            .extractor
            .extract_embedding(image_bytes, bbox, landmarks)
            .await
            .context("face embedding extraction failed")?
        {
            Some(e) => e,
            None => {
                debug!("identify_face: no face embedding (quality gate)");
                // Landmarks still matter: liveness needs to know a face was detected.
                return Ok(FaceIdentificationDetails {
                    identification: FaceIdentification::no_face(),
                    landmarks,
                    embedding: None,
                    bbox,
                });
            }
        };

        let dims = self.extractor.embedding_dims() as i64;
        let rows: Vec<FaceRow> = sqlx::query_as(
            "SELECT id, profile_id, embedding, model_dims, created_at \
             FROM face_embeddings WHERE model_dims = ?",
        )
        .bind(dims)
        .fetch_all(&self.pool)
        .await
        .context("failed to load face embeddings for matching")?;

        // Every return below carries the landmarks and embedding, whatever the verdict.
        let with_diag = |identification: FaceIdentification| FaceIdentificationDetails {
            identification,
            landmarks,
            embedding: Some(query_embedding.clone()),
            bbox,
        };

        if rows.is_empty() {
            return Ok(with_diag(FaceIdentification::unknown(None)));
        }

        // Raw embeddings are kept, not just scores, for each profile's centroid.
        let mut by_profile: HashMap<String, Vec<(Vec<f32>, f32)>> = HashMap::new();
        let mut raw_row_scores: Vec<(String, String, f32)> = Vec::new();
        for row in rows {
            let candidate = match unpack_embedding(&row.embedding) {
                Ok(v) => v,
                Err(e) => {
                    warn!(id = %row.id, "skipping malformed embedding: {e}");
                    continue;
                }
            };
            let score = cosine_similarity(&query_embedding, &candidate);
            raw_row_scores.push((row.profile_id.clone(), row.id.clone(), score));
            by_profile
                .entry(row.profile_id)
                .or_default()
                .push((candidate, score));
        }
        // If a stranger scores 0.75+ here, suspect the embedder or enrollments, not the gates.
        info!(
            scores = ?raw_row_scores,
            "identify: raw cosines vs every enrollment"
        );

        let per_profile: Vec<(String, f32)> = by_profile
            .into_iter()
            .filter(|(_, items)| items.len() >= self.min_samples_to_identify)
            .map(|(pid, items)| {
                let mut scores: Vec<f32> = items.iter().map(|(_, s)| *s).collect();
                scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                let k = scores.len().min(TOP_K);
                let topk_mean = scores.iter().take(k).sum::<f32>() / k as f32;

                let embeddings: Vec<&Vec<f32>> = items.iter().map(|(v, _)| v).collect();
                let centroid_cos = if let Some(c) = centroid_unit(&embeddings) {
                    cosine_similarity(&query_embedding, &c)
                } else {
                    topk_mean
                };
                let combined = CENTROID_WEIGHT * centroid_cos + (1.0 - CENTROID_WEIGHT) * topk_mean;
                (pid, combined)
            })
            .collect();

        if per_profile.is_empty() {
            return Ok(with_diag(FaceIdentification::unknown(None)));
        }

        // ── S-norm (query-side) ─────────────────────────────────────────
        // A blank frame matching everyone at ~0.5 collapses toward zero; a true match survives.
        let total: f32 = per_profile.iter().map(|(_, s)| *s).sum();
        let n_profiles = per_profile.len() as f32;
        let snormed: Vec<(String, f32)> = per_profile
            .iter()
            .map(|(pid, s)| {
                let mean_other = if n_profiles > 1.0 {
                    (total - s) / (n_profiles - 1.0)
                } else {
                    0.0
                };
                (pid.clone(), s - SNORM_WEIGHT * mean_other)
            })
            .collect();

        // Rank by normalised score but report raw, which is what the threshold is tuned against.
        let mut ranked: Vec<(String, f32, f32)> = per_profile
            .iter()
            .zip(snormed.iter())
            .map(|((pid, raw), (_, norm))| (pid.clone(), *raw, *norm))
            .collect();
        ranked.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let (best_pid, best_raw, best_norm) = ranked[0].clone();
        let runner_up_norm = ranked.get(1).map(|r| r.2).unwrap_or(-1.0);
        let margin = best_norm - runner_up_norm;
        let confidence = best_raw.max(0.0);

        let gap_to_mean = if ranked.len() > 1 {
            let other_sum: f32 = ranked.iter().skip(1).map(|r| r.2).sum();
            let other_mean = other_sum / (ranked.len() - 1) as f32;
            best_norm - other_mean
        } else {
            // One profile: no gap to measure, so pass this gate; the threshold still applies.
            self.open_set_gap_min
        };

        // Best effort: a failed override lookup falls back to the global threshold.
        let base_threshold = match self.lookup_profile_threshold(&best_pid).await {
            Ok(Some(t)) => {
                debug!(%best_pid, override_threshold = t, "applying per-profile threshold");
                t
            }
            Ok(None) => self.threshold,
            Err(e) => {
                warn!(%best_pid, "per-profile threshold lookup failed ({e:#}); using global");
                self.threshold
            }
        };

        // With one profile the cross-profile gates are no-ops, so add `single_profile_margin`,
        // unless the operator pinned the threshold (compressed-cosine exports need ~0.996).
        let effective_threshold = if ranked.len() <= 1 && !self.threshold_is_user_set {
            (base_threshold + self.single_profile_margin).min(0.99)
        } else {
            base_threshold
        };

        let passes_threshold = best_raw >= effective_threshold;
        let passes_runner_up = margin >= self.runner_up_margin;
        let passes_open_set = gap_to_mean >= self.open_set_gap_min;

        // INFO on purpose: operators diagnose field false positives from this.
        info!(
            %best_pid,
            confidence = best_raw,
            best_norm,
            margin,
            gap_to_mean,
            threshold = effective_threshold,
            runner_up_margin_required = self.runner_up_margin,
            open_set_gap_required = self.open_set_gap_min,
            passes_threshold,
            passes_runner_up,
            passes_open_set,
            n_profiles = ranked.len(),
            single_profile_mode = ranked.len() <= 1,
            "face identify decision"
        );

        if passes_threshold && passes_runner_up && passes_open_set {
            Ok(with_diag(FaceIdentification::found(best_pid, confidence)))
        } else {
            Ok(with_diag(FaceIdentification::unknown(Some(confidence))))
        }
    }

    async fn list_embeddings(&self, profile_id: &str) -> Result<Vec<FaceEmbedding>> {
        let rows: Vec<FaceRow> = sqlx::query_as(
            "SELECT id, profile_id, embedding, model_dims, created_at \
             FROM face_embeddings WHERE profile_id = ? \
             ORDER BY created_at DESC, id DESC",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list face embeddings")?;
        rows.into_iter().map(row_to_embedding).collect()
    }

    async fn delete_embeddings(&self, profile_id: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM face_embeddings WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.pool)
            .await
            .context("failed to delete face embeddings")?;
        let n = result.rows_affected();
        info!(%profile_id, deleted = n, "face embeddings deleted");
        Ok(n)
    }

    fn match_threshold(&self) -> f32 {
        self.threshold
    }

    async fn get_profile_threshold(&self, profile_id: &str) -> Result<Option<f32>> {
        self.lookup_profile_threshold(profile_id).await
    }

    async fn set_profile_threshold(
        &self,
        profile_id: &str,
        threshold: Option<f32>,
        note: Option<&str>,
    ) -> Result<()> {
        SqliteFaceRecognition::set_profile_threshold(self, profile_id, threshold, note).await
    }

    async fn pairwise_similarities(&self) -> Result<Vec<PairwiseSimilarity>> {
        let rows: Vec<FaceRow> = sqlx::query_as(
            "SELECT id, profile_id, embedding, model_dims, created_at \
             FROM face_embeddings ORDER BY profile_id, id",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to load face embeddings for pairwise diag")?;

        // Decode embeddings once.
        let mut items: Vec<(String, String, Vec<f32>)> = Vec::with_capacity(rows.len());
        for row in rows {
            match unpack_embedding(&row.embedding) {
                Ok(v) => items.push((row.id, row.profile_id, v)),
                Err(e) => warn!("pairwise: skip malformed embedding: {e}"),
            }
        }

        let mut out = Vec::with_capacity(items.len() * items.len() / 2);
        for i in 0..items.len() {
            for j in (i + 1)..items.len() {
                let sim = cosine_similarity(&items[i].2, &items[j].2);
                out.push(PairwiseSimilarity {
                    id_a: items[i].0.clone(),
                    id_b: items[j].0.clone(),
                    profile_a: items[i].1.clone(),
                    profile_b: items[j].1.clone(),
                    similarity: sim,
                    same_profile: items[i].1 == items[j].1,
                });
            }
        }
        Ok(out)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_profile::SqliteProfileRepository;
    use async_trait::async_trait;
    use pond_core::user_data::domain::profile::CreateProfileRequest;
    use pond_core::user_data::ports::profile::ProfileRepository;
    use tempfile::tempdir;

    /// Deterministic stub extractor.
    struct StubExtractor {
        dims: u32,
    }

    #[async_trait]
    impl FaceEmbeddingExtractor for StubExtractor {
        async fn extract_embedding(
            &self,
            image_bytes: &[u8],
            _bbox: Option<BoundingBox>,
            _landmarks: Option<FaceLandmarks>,
        ) -> Result<Option<Vec<f32>>> {
            if image_bytes.is_empty() {
                return Ok(None);
            }
            let mut v = vec![0.0_f32; self.dims as usize];
            let slot = (image_bytes[0] as usize) % (self.dims as usize);
            v[slot] = 1.0;
            Ok(Some(v))
        }
        fn embedding_dims(&self) -> u32 {
            self.dims
        }
    }

    async fn setup() -> (SqliteFaceRecognition, String, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let profiles = SqliteProfileRepository::new(db.system.clone());
        let profile = profiles
            .create(CreateProfileRequest {
                display_name: "Alice".into(),
                avatar_emoji: "\u{1F986}".into(),
            })
            .await
            .unwrap();
        let svc = SqliteFaceRecognition::new(db.system, Arc::new(StubExtractor { dims: 128 }));
        (svc, profile.id, tmp)
    }

    #[tokio::test]
    async fn register_and_list_round_trip() {
        let (svc, profile_id, _tmp) = setup().await;
        let stored = svc
            .register_face(&profile_id, &[7u8, 1, 2], None)
            .await
            .unwrap();
        assert_eq!(stored.profile_id, profile_id);
        assert_eq!(stored.embedding.len(), 128);
        let listed = svc.list_embeddings(&profile_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, stored.id);
    }

    #[tokio::test]
    async fn identify_returns_enrolled_profile_once_min_samples_met() {
        let (svc, profile_id, _tmp) = setup().await;
        svc.register_face(&profile_id, &[42u8], None).await.unwrap();
        svc.register_face(&profile_id, &[42u8, 5], None)
            .await
            .unwrap();
        svc.register_face(&profile_id, &[42u8, 7, 3], None)
            .await
            .unwrap();

        let result = svc.identify_face(&[42u8, 99, 100], None).await.unwrap();
        assert!(result.identified, "expected positive identification");
        assert_eq!(result.profile_id.as_deref(), Some(profile_id.as_str()));
        assert!(result.confidence.unwrap() >= 0.6);
    }

    #[tokio::test]
    async fn identify_rejects_single_sample_profiles_when_min_samples_configured() {
        let (svc, profile_id, _tmp) = setup().await;
        let svc = SqliteFaceRecognition {
            pool: svc.pool.clone(),
            extractor: svc.extractor.clone(),
            detector: svc.detector.clone(),
            model_name: svc.model_name.clone(),
            threshold: svc.threshold,
            runner_up_margin: svc.runner_up_margin,
            open_set_gap_min: svc.open_set_gap_min,
            min_samples_to_identify: 3,
            single_profile_margin: svc.single_profile_margin,
            threshold_is_user_set: svc.threshold_is_user_set,
        };
        svc.register_face(&profile_id, &[42u8], None).await.unwrap();
        let result = svc.identify_face(&[42u8, 9], None).await.unwrap();
        assert!(!result.identified);
    }

    #[tokio::test]
    async fn identify_returns_unknown_below_threshold() {
        let (svc, profile_id, _tmp) = setup().await;
        // Enough enrollments that only the threshold can reject.
        svc.register_face(&profile_id, &[1u8], None).await.unwrap();
        svc.register_face(&profile_id, &[1u8, 9], None)
            .await
            .unwrap();
        svc.register_face(&profile_id, &[1u8, 9, 7], None)
            .await
            .unwrap();
        // First byte differs → orthogonal embedding → similarity ≈ 0
        let result = svc.identify_face(&[200u8], None).await.unwrap();
        assert!(!result.identified);
        assert!(result.profile_id.is_none());
    }

    #[tokio::test]
    async fn identify_without_enrollments_returns_unknown() {
        let (svc, _profile_id, _tmp) = setup().await;
        let result = svc.identify_face(&[1u8], None).await.unwrap();
        assert!(!result.identified);
        assert!(result.profile_id.is_none());
    }

    #[tokio::test]
    async fn identify_returns_no_face_on_empty_image() {
        let (svc, profile_id, _tmp) = setup().await;
        svc.register_face(&profile_id, &[1u8], None).await.unwrap();
        svc.register_face(&profile_id, &[1u8, 2], None)
            .await
            .unwrap();
        let result = svc.identify_face(&[], None).await.unwrap();
        assert!(!result.identified);
        assert!(result.confidence.is_none());
    }

    #[tokio::test]
    async fn delete_removes_all_embeddings_for_profile() {
        let (svc, profile_id, _tmp) = setup().await;
        svc.register_face(&profile_id, &[1u8], None).await.unwrap();
        svc.register_face(&profile_id, &[2u8], None).await.unwrap();
        svc.register_face(&profile_id, &[3u8], None).await.unwrap();
        let n = svc.delete_embeddings(&profile_id).await.unwrap();
        assert_eq!(n, 3);
        assert!(svc.list_embeddings(&profile_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pairwise_similarities_emits_all_unordered_pairs() {
        let (svc, profile_id, _tmp) = setup().await;
        svc.register_face(&profile_id, &[1u8], None).await.unwrap();
        svc.register_face(&profile_id, &[1u8, 9], None)
            .await
            .unwrap();
        svc.register_face(&profile_id, &[2u8], None).await.unwrap();
        let pairs = svc.pairwise_similarities().await.unwrap();
        // 3 rows → C(3, 2) = 3 pairs.
        assert_eq!(pairs.len(), 3);
        assert!(pairs.iter().any(|p| p.same_profile));
    }

    #[test]
    fn pack_unpack_is_lossless() {
        let v = vec![0.0_f32, 1.5, -3.25, std::f32::consts::PI];
        let bytes = pack_embedding(&v);
        let back = unpack_embedding(&bytes).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn centroid_unit_averages_and_normalises() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        let c = centroid_unit(&[&a, &b]).unwrap();
        // Centroid is (0.5, 0.5) → normalised to (√2/2, √2/2).
        let expected = std::f32::consts::FRAC_1_SQRT_2;
        assert!((c[0] - expected).abs() < 1e-5 && (c[1] - expected).abs() < 1e-5);
    }

    #[test]
    fn centroid_unit_rejects_dimension_mismatch() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        assert!(centroid_unit(&[&a, &b]).is_none());
    }

    #[test]
    fn pose_gate_accepts_frontal_level_face() {
        let lms = FaceLandmarks {
            left_eye: (40.0, 50.0),
            right_eye: (80.0, 50.0),
            nose: (60.0, 70.0),
            left_mouth: (45.0, 90.0),
            right_mouth: (75.0, 90.0),
        };
        assert!(check_pose_sane(&lms).is_ok());
    }

    #[test]
    fn pose_gate_rejects_tilted_face() {
        // 45° tilt — way beyond 25° cutoff.
        let lms = FaceLandmarks {
            left_eye: (40.0, 50.0),
            right_eye: (80.0, 90.0),
            nose: (60.0, 70.0),
            left_mouth: (45.0, 110.0),
            right_mouth: (75.0, 110.0),
        };
        assert!(check_pose_sane(&lms).is_err());
    }

    #[test]
    fn pose_gate_rejects_profile_shot() {
        // Nose way to the left of both eyes.
        let lms = FaceLandmarks {
            left_eye: (40.0, 50.0),
            right_eye: (80.0, 50.0),
            nose: (10.0, 70.0),
            left_mouth: (45.0, 90.0),
            right_mouth: (75.0, 90.0),
        };
        assert!(check_pose_sane(&lms).is_err());
    }

    #[test]
    fn cosine_similarity_basic_cases() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[], &[0.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }
}
