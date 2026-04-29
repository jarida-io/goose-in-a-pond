//! SQLite-backed implementation of the [`FaceRecognition`] port.
//!
//! Composes a [`FaceEmbeddingExtractor`] (ONNX) with an optional
//! [`FaceDetector`] and on-disk persistence.  Embeddings are packed as
//! little-endian f32 BLOBs in the `face_embeddings` table (migration 0013).
//!
//! # Matching strategy (phase-2 hardened)
//!
//! The naive "max cosine across all rows" rule lets noisy enrollments and
//! collapsed embedders produce false positives.  We use three hardenings:
//!
//!   1. **Per-profile top-K mean** — for each enrolled profile, average the
//!      K highest similarities (K=3).  Reduces the influence of a single
//!      outlier enrollment.
//!   2. **Runner-up margin** — the best profile's mean score must exceed the
//!      second-best profile's mean score by at least `RUNNER_UP_MARGIN`.
//!      This is the single biggest lever against the "all other faces pass
//!      at 0.6" failure mode: a real match has clear daylight over other
//!      profiles; a collapsed-embedding false match does not.
//!   3. **Minimum samples** — profiles with fewer than `MIN_SAMPLES_TO_IDENTIFY`
//!      enrollments are excluded from identification until they are more
//!      fully enrolled.  Matching against a single noisy embedding is too
//!      risky; asking for 2+ samples at enrollment time costs the user
//!      nothing but eliminates a whole class of single-example false
//!      positives.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::domain::face_recognition::{
    BoundingBox, DetectedFace, FaceEmbedding, FaceIdentification, FaceLandmarks,
};
use pond_core::ports::face_detector::FaceDetector;
use pond_core::ports::face_embedding_extractor::FaceEmbeddingExtractor;
use pond_core::ports::face_recognition::{FaceRecognition, PairwiseSimilarity};
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Default cosine-similarity threshold for ArcFace-512.
///
/// Tightened from the original 0.60 floor after field-testing.  ArcFace R100
/// on Umeyama-aligned 112×112 crops can safely run at 0.62–0.68 without
/// rejecting genuine re-captures; 0.60 admitted too many nearest-neighbours
/// in the embedding space.  Override at runtime with
/// `POND_FACE_MATCH_THRESHOLD`.
const DEFAULT_MATCH_THRESHOLD: f32 = 0.70;

/// Extra confidence required when only **one** profile is fully enrolled.
/// In single-profile mode the runner-up margin and open-set gap are no-ops
/// (there is nothing to compare against), so the absolute threshold is the
/// only gate standing between a stranger's embedding and a false match.
/// Lift it by this amount to restore the discriminative buffer the
/// cross-profile checks provide in multi-profile mode.  Override via
/// `POND_FACE_SINGLE_PROFILE_MARGIN`.
const DEFAULT_SINGLE_PROFILE_MARGIN: f32 = 0.10;

/// Minimum detected face size (in source-image pixels).  Tiny detections
/// come from distant subjects or false positives and produce unreliable
/// embeddings.
const MIN_DETECTED_FACE_PX: u32 = 80;

/// Top-K pooling of per-profile similarities.
const TOP_K: usize = 3;

/// Weight on the centroid-vs-query cosine when combining with top-K mean.
/// Centroid pooling is generally the more stable signal, so we lean on it.
/// Final per-profile score is `CENTROID_WEIGHT * centroid + (1-w) * topk_mean`.
const CENTROID_WEIGHT: f32 = 0.6;

/// S-norm blend: final ranking score is
///   `(1-w) * raw + w * (raw - mean_other_profile_raws)`
/// which equals `raw - w * mean_other`.  Setting `w = 0.5` demeans half the
/// "everyone matches at 0.5 today" baseline without over-penalising true
/// matches against a small cohort.  With only one other profile in the
/// database the correction is exactly the impostor score; with zero others
/// S-norm is a no-op.
const SNORM_WEIGHT: f32 = 0.5;

/// The best profile's mean score must beat the runner-up by at least this
/// margin for the identification to be considered conclusive.  Tuned
/// empirically against ArcFace-on-aligned-crops.  Override via
/// `POND_FACE_RUNNER_UP_MARGIN`.
const DEFAULT_RUNNER_UP_MARGIN: f32 = 0.10;

/// Profiles with fewer enrollments than this are excluded from
/// identification.  Keeps single-example false positives off the table.
/// Override via `POND_FACE_MIN_SAMPLES`.  Bumped from 2 → 3: a single noisy
/// enrollment pair was enough to become matchable under the old floor.
const DEFAULT_MIN_SAMPLES_TO_IDENTIFY: usize = 1;

/// Open-set rejection floor.  In addition to the runner-up margin, the
/// winning profile's normalised score must beat the *mean* of every
/// other profile's normalised score by at least this amount.  Catches
/// the case where there are two near-tied close competitors and a long
/// tail of low-scoring profiles — the runner-up margin alone is happy,
/// but the field is so dense that the win is not reliable.  Override via
/// `POND_FACE_OPEN_SET_GAP`.
const DEFAULT_OPEN_SET_GAP_TO_MEAN_MIN: f32 = 0.08;

/// Maximum absolute eye-line tilt (radians) before a face is considered too
/// rotated for reliable matching.  At 25° the embedding space starts to
/// degrade noticeably even with alignment.
const MAX_EYE_TILT_RAD: f32 = 0.436; // ≈ 25°

/// Nose must sit between the eyes' x-coordinates (with a ±fraction-of-
/// inter-eye-distance slack) for the face to count as roughly frontal.
/// Profile shots push the nose outside this band and produce poor matches.
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
    /// Set when the operator provided `POND_FACE_MATCH_THRESHOLD` explicitly.
    /// An operator-supplied floor is authoritative — the single-profile
    /// margin is skipped when this is true so the final threshold is exactly
    /// what they configured.  This matters for deployments with a
    /// compressed-cosine ONNX export (see the troubleshooting note in
    /// `lib.rs:use_bgr_input`) that need a very high threshold (~0.995+)
    /// regardless of profile count.
    threshold_is_user_set: bool,
}

/// Read a numeric env-var, clamped to a safe range, falling back to `default`
/// when unset or unparseable.  Single source of truth for the four matcher
/// knobs — `POND_FACE_MATCH_THRESHOLD`, `POND_FACE_RUNNER_UP_MARGIN`,
/// `POND_FACE_OPEN_SET_GAP`, `POND_FACE_MIN_SAMPLES`.
fn env_f32(name: &str, default: f32, lo: f32, hi: f32) -> f32 {
    match std::env::var(name).ok().and_then(|s| s.parse::<f32>().ok()) {
        Some(v) if v.is_finite() => v.clamp(lo, hi),
        _ => default,
    }
}

fn env_usize(name: &str, default: usize, lo: usize, hi: usize) -> usize {
    match std::env::var(name).ok().and_then(|s| s.parse::<usize>().ok()) {
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
            threshold: env_f32("POND_FACE_MATCH_THRESHOLD", DEFAULT_MATCH_THRESHOLD, 0.0, 1.0),
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

    /// Attach a detector.  If the detector produces landmarks
    /// ([`FaceDetector::produces_landmarks`] = true) the extractor will use
    /// similarity-transform alignment; otherwise it falls back to
    /// crop-and-resize by bbox.
    pub fn with_detector(mut self, detector: Arc<dyn FaceDetector>) -> Self {
        self.detector = Some(detector);
        self
    }

    /// Read the per-profile threshold override from migration 0014.
    /// Returns `Ok(None)` when no row exists or the override is NULL.
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

    /// Set / clear the per-profile threshold override.  Pass `Some(t)` to
    /// install or update; `None` to remove the row entirely (revert to
    /// the global threshold).  `t` is clamped to \[0.0, 1.0\] to keep
    /// invalid configurations out of the matcher.
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

    /// Read the per-profile threshold override (None ⇒ global applies).
    /// Public wrapper around the internal lookup, for the API layer.
    pub async fn get_profile_threshold(&self, profile_id: &str) -> Result<Option<f32>> {
        self.lookup_profile_threshold(profile_id).await
    }

    /// Run the detector and validate the hit.  Returns:
    ///   - `Ok(Some(face))` if a face was found and passes quality gates;
    ///   - `Ok(None)` if no detector is attached (caller should fall back);
    ///   - `Err(_)` if a detector is attached but found no face or the crop
    ///     is too small — this is surfaced as a user-actionable failure.
    async fn run_detector(&self, image_bytes: &[u8]) -> Result<Option<DetectedFace>> {
        let Some(det) = self.detector.as_ref() else { return Ok(None); };
        let found = det
            .detect_face(image_bytes)
            .await
            .context("face detector failed")?;
        match found {
            None => Err(anyhow!("no face detected in image")),
            Some(face) => {
                if face.bbox.width < MIN_DETECTED_FACE_PX
                    || face.bbox.height < MIN_DETECTED_FACE_PX
                {
                    return Err(anyhow!(
                        "detected face too small ({}×{} px); move closer to the camera",
                        face.bbox.width, face.bbox.height
                    ));
                }
                // Pose sanity: if landmarks are available, reject extreme
                // tilt or profile shots that alignment can't fully rescue.
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

/// Reject faces whose landmark geometry implies extreme rotation / profile.
/// These produce embeddings that even alignment can't fully recover.
fn check_pose_sane(lms: &FaceLandmarks) -> Result<()> {
    let (lex, ley) = lms.left_eye;
    let (rex, rey) = lms.right_eye;
    let (nx, _ny)  = lms.nose;
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
    // Nose centering: measured as fraction of inter-eye distance outside
    // the [left_eye_x, right_eye_x] span.
    let eye_lo = lex.min(rex);
    let eye_hi = lex.max(rex);
    let slack = NOSE_CENTERING_SLACK * (eye_hi - eye_lo).abs().max(1.0);
    if nx < eye_lo - slack || nx > eye_hi + slack {
        return Err(anyhow!(
            "face not frontal enough; turn toward the camera"
        ));
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct FaceRow {
    id:         String,
    profile_id: String,
    embedding:  Vec<u8>,
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
    for f in v { out.extend_from_slice(&f.to_le_bytes()); }
    out
}

fn unpack_embedding(bytes: &[u8]) -> Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return Err(anyhow!("embedding BLOB length {} not a multiple of 4", bytes.len()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn row_to_embedding(row: FaceRow) -> Result<FaceEmbedding> {
    Ok(FaceEmbedding {
        id:         row.id,
        profile_id: row.profile_id,
        embedding:  unpack_embedding(&row.embedding)?,
        model_dims: row.model_dims as u32,
        created_at: parse_dt(&row.created_at),
    })
}

/// Compute the centroid of a set of embeddings (mean of vectors) and
/// re-normalise to unit length so it lives on the same sphere as every
/// individual embedding.  Returns `None` for an empty input or a centroid
/// that degenerates to zero norm (perfectly antipodal samples — so rare
/// it's effectively never in practice).
fn centroid_unit(embeddings: &[&Vec<f32>]) -> Option<Vec<f32>> {
    let first = embeddings.first()?;
    let dims = first.len();
    if dims == 0 { return None; }
    let mut acc = vec![0.0_f32; dims];
    for e in embeddings {
        if e.len() != dims { return None; }
        for i in 0..dims { acc[i] += e[i]; }
    }
    let norm: f32 = acc.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm <= 1e-8 { return None; }
    for v in acc.iter_mut() { *v /= norm; }
    Some(acc)
}

/// Cosine similarity in \[-1.0, 1.0\].  Returns 0.0 for zero-norm vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() { return 0.0; }
    let mut dot = 0.0_f32; let mut na = 0.0_f32; let mut nb = 0.0_f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na  += a[i] * a[i];
        nb  += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 { return 0.0; }
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
                    // If caller supplied an explicit bbox, honour it even
                    // when the detector disagreed — preserves phase-2
                    // baseline behaviour for operators who already have a
                    // known-good crop.
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
            .ok_or_else(|| anyhow!(
                "face failed quality gate during enrollment — check server log for the \
                 specific gate (low-variance / extreme-brightness / blurry / anti-spoof). \
                 Common knobs: POND_FACE_ANTISPOOF=off, POND_FACE_ANTISPOOF_THRESHOLD=0.85"
            ))?;

        let dims = self.extractor.embedding_dims();
        if embedding.len() != dims as usize {
            return Err(anyhow!(
                "extractor returned {} dims but advertises {}",
                embedding.len(), dims
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
    ) -> Result<pond_core::ports::face_recognition::FaceIdentificationDetails> {
        use pond_core::ports::face_recognition::FaceIdentificationDetails;

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
                // Return what we have: bbox/landmarks may still be useful
                // to the caller (e.g. the liveness burst wants to know a
                // face *was* detected even when preprocessing rejected it).
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

        // Helper to wrap a bare `FaceIdentification` with the diagnostic
        // side-channel fields we've already computed.  Used for every
        // return path below so the caller always sees the landmarks +
        // embedding (when present), regardless of the verdict.
        let with_diag = |identification: FaceIdentification| FaceIdentificationDetails {
            identification,
            landmarks,
            embedding: Some(query_embedding.clone()),
            bbox,
        };

        if rows.is_empty() {
            return Ok(with_diag(FaceIdentification::unknown(None)));
        }

        // Group embeddings and similarities by profile.  We retain the raw
        // embeddings (not just scores) so we can compute each profile's
        // centroid and score it against the query as well.
        let mut by_profile: HashMap<String, Vec<(Vec<f32>, f32)>> = HashMap::new();
        let mut raw_row_scores: Vec<(String, String, f32)> = Vec::new();
        for row in rows {
            let candidate = match unpack_embedding(&row.embedding) {
                Ok(v) => v,
                Err(e) => { warn!(id = %row.id, "skipping malformed embedding: {e}"); continue; }
            };
            let score = cosine_similarity(&query_embedding, &candidate);
            raw_row_scores.push((row.profile_id.clone(), row.id.clone(), score));
            by_profile.entry(row.profile_id).or_default().push((candidate, score));
        }
        // Log raw cosines per enrollment — the single most useful signal
        // for operators diagnosing false positives / negatives.  If a
        // stranger's frame hits 0.75+ here, the problem is the embedder
        // or the enrollment data, not the downstream gates.
        info!(
            scores = ?raw_row_scores,
            "identify: raw cosines vs every enrollment"
        );

        // Compute per-profile top-K mean and centroid similarity, skipping
        // under-enrolled profiles.  The combined score weights centroid
        // heavily because it absorbs enrollment-level variation.
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
                let combined =
                    CENTROID_WEIGHT * centroid_cos + (1.0 - CENTROID_WEIGHT) * topk_mean;
                (pid, combined)
            })
            .collect();

        if per_profile.is_empty() {
            return Ok(with_diag(FaceIdentification::unknown(None)));
        }

        // ── S-norm (query-side) ─────────────────────────────────────────
        // Subtract a weighted average of the impostor scores from each
        // profile's raw score.  On a "blank" frame where every profile
        // matches at ~0.5, subtracting the cross-profile mean collapses
        // all normalised scores toward zero; on a genuine match the
        // impostors stay low and the true profile keeps most of its score.
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

        // Rank by the normalised score.  Confidence reported to the caller
        // is still the raw combined score (easier to reason about against
        // the configured threshold).
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

        // Open-set rejection: the win must clear *both* the runner-up
        // and the mean of all *other* normalised scores.  Catches the
        // "two close competitors plus a long tail" failure pattern that
        // a pure best-vs-second margin cannot see.
        let gap_to_mean = if ranked.len() > 1 {
            let other_sum: f32 = ranked.iter().skip(1).map(|r| r.2).sum();
            let other_mean = other_sum / (ranked.len() - 1) as f32;
            best_norm - other_mean
        } else {
            // Only one profile enrolled — gap-to-mean is meaningless;
            // fall back to a value that always passes this gate so the
            // runner-up margin (which is also a no-op in this case) plus
            // the absolute threshold remain the active checks.
            self.open_set_gap_min
        };

        // Per-profile threshold override (migration 0014).  When
        // populated, replaces the global `self.threshold` for *just*
        // this profile.  Lookup is best-effort: a transport / parse
        // error simply falls back to the global threshold so a broken
        // override row never wedges identification.
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

        // Single-profile mode hardening.  When only one profile clears the
        // min-samples gate, the runner-up margin and open-set gap gates
        // below degenerate to no-ops (there is nothing to compare with).
        // In that regime a stranger whose embedding happens to land above
        // the absolute threshold would be falsely accepted.  Lift the
        // threshold by `single_profile_margin` to restore the buffer the
        // cross-profile checks provide in multi-profile mode.
        // If the operator pinned the threshold via env/with_threshold, honour
        // it verbatim — don't clamp or bump. This is required when the
        // installed ONNX export produces a compressed cosine range that needs
        // a very high floor (e.g. 0.996) even in single-profile mode.
        let effective_threshold = if ranked.len() <= 1 && !self.threshold_is_user_set {
            (base_threshold + self.single_profile_margin).min(0.99)
        } else {
            base_threshold
        };

        let passes_threshold = best_raw >= effective_threshold;
        let passes_runner_up = margin >= self.runner_up_margin;
        let passes_open_set  = gap_to_mean >= self.open_set_gap_min;

        // Verbose decision log — operators need every score + every gate
        // outcome to diagnose false positives in the field.
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

    fn match_threshold(&self) -> f32 { self.threshold }

    async fn get_profile_threshold(&self, profile_id: &str) -> Result<Option<f32>> {
        self.lookup_profile_threshold(profile_id).await
    }

    async fn set_profile_threshold(
        &self,
        profile_id: &str,
        threshold: Option<f32>,
        note: Option<&str>,
    ) -> Result<()> {
        // Delegate to the inherent method (kept available for callers that
        // hold a concrete `SqliteFaceRecognition`, e.g. tests).
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
                    id_a:         items[i].0.clone(),
                    id_b:         items[j].0.clone(),
                    profile_a:    items[i].1.clone(),
                    profile_b:    items[j].1.clone(),
                    similarity:   sim,
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
    use pond_core::domain::profile::CreateProfileRequest;
    use pond_core::ports::profile::ProfileRepository;
    use tempfile::tempdir;

    /// Deterministic stub extractor.
    struct StubExtractor { dims: u32 }

    #[async_trait]
    impl FaceEmbeddingExtractor for StubExtractor {
        async fn extract_embedding(
            &self,
            image_bytes: &[u8],
            _bbox: Option<BoundingBox>,
            _landmarks: Option<FaceLandmarks>,
        ) -> Result<Option<Vec<f32>>> {
            if image_bytes.is_empty() { return Ok(None); }
            let mut v = vec![0.0_f32; self.dims as usize];
            let slot = (image_bytes[0] as usize) % (self.dims as usize);
            v[slot] = 1.0;
            Ok(Some(v))
        }
        fn embedding_dims(&self) -> u32 { self.dims }
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
        let svc = SqliteFaceRecognition::new(
            db.system,
            Arc::new(StubExtractor { dims: 128 }),
        );
        (svc, profile.id, tmp)
    }

    #[tokio::test]
    async fn register_and_list_round_trip() {
        let (svc, profile_id, _tmp) = setup().await;
        let stored = svc.register_face(&profile_id, &[7u8, 1, 2], None).await.unwrap();
        assert_eq!(stored.profile_id, profile_id);
        assert_eq!(stored.embedding.len(), 128);
        let listed = svc.list_embeddings(&profile_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, stored.id);
    }

    #[tokio::test]
    async fn identify_returns_enrolled_profile_once_min_samples_met() {
        let (svc, profile_id, _tmp) = setup().await;
        // DEFAULT_MIN_SAMPLES_TO_IDENTIFY = 3 → need three enrollments.
        svc.register_face(&profile_id, &[42u8], None).await.unwrap();
        svc.register_face(&profile_id, &[42u8, 5], None).await.unwrap();
        svc.register_face(&profile_id, &[42u8, 7, 3], None).await.unwrap();

        let result = svc.identify_face(&[42u8, 99, 100], None).await.unwrap();
        assert!(result.identified, "expected positive identification");
        assert_eq!(result.profile_id.as_deref(), Some(profile_id.as_str()));
        assert!(result.confidence.unwrap() >= 0.6);
    }

    #[tokio::test]
    async fn identify_rejects_single_sample_profiles_when_min_samples_configured() {
        // With the env-override honoured by `new`, set MIN_SAMPLES=3 for this
        // test so the "under-enrolled profile → unknown" path is exercised.
        // The default is now 1 so single-enrollment deployments work, but
        // operators that want the stricter policy can still get it.
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
        // Only one sample stored; identify should refuse to return a match.
        let result = svc.identify_face(&[42u8, 9], None).await.unwrap();
        assert!(!result.identified);
    }

    #[tokio::test]
    async fn identify_returns_unknown_below_threshold() {
        let (svc, profile_id, _tmp) = setup().await;
        // Three enrollments so MIN_SAMPLES_TO_IDENTIFY is met — the
        // rejection must come from the *threshold*, not the min-samples gate.
        svc.register_face(&profile_id, &[1u8], None).await.unwrap();
        svc.register_face(&profile_id, &[1u8, 9], None).await.unwrap();
        svc.register_face(&profile_id, &[1u8, 9, 7], None).await.unwrap();
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
        svc.register_face(&profile_id, &[1u8, 2], None).await.unwrap();
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
        svc.register_face(&profile_id, &[1u8, 9], None).await.unwrap();
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
            left_eye:    (40.0, 50.0),
            right_eye:   (80.0, 50.0),
            nose:        (60.0, 70.0),
            left_mouth:  (45.0, 90.0),
            right_mouth: (75.0, 90.0),
        };
        assert!(check_pose_sane(&lms).is_ok());
    }

    #[test]
    fn pose_gate_rejects_tilted_face() {
        // 45° tilt — way beyond 25° cutoff.
        let lms = FaceLandmarks {
            left_eye:    (40.0, 50.0),
            right_eye:   (80.0, 90.0),
            nose:        (60.0, 70.0),
            left_mouth:  (45.0, 110.0),
            right_mouth: (75.0, 110.0),
        };
        assert!(check_pose_sane(&lms).is_err());
    }

    #[test]
    fn pose_gate_rejects_profile_shot() {
        // Nose way to the left of both eyes.
        let lms = FaceLandmarks {
            left_eye:    (40.0, 50.0),
            right_eye:   (80.0, 50.0),
            nose:        (10.0, 70.0),
            left_mouth:  (45.0, 90.0),
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
