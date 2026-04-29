//! Speaker identification adapter — in-process ONNX x-vector model.
//!
//! Replaces the former Resemblyzer HTTP bridge with a pure-Rust pipeline:
//!
//! ```text
//! WAV bytes (16 kHz mono)
//!   → log-mel filterbank  [T × 40]
//!   → ONNX x-vector TDNN
//!   → Vec<f32>  (512-d embedding)
//!   → little-endian BLOB  → speaker_embeddings table
//! ```
//!
//! # Model
//! Tested against the SpeechBrain `spkrec-xvect-voxceleb` ONNX export:
//! - Input tensor  : `"feats"`     — shape `[1, T, 24]` float32
//! - Output tensor : `"embedding"` — shape `[1, 512]` float32
//!
//! Export the model once (requires Python + speechbrain):
//! ```bash
//! python scripts/export_xvector.py --output ~/.pond/models/speaker.onnx
//! ```
//!
//! # Shipping
//! The `ort` crate links against `libonnxruntime` dynamically.
//! Bundle the ONNX Runtime shared library alongside your binary:
//! - macOS : `libonnxruntime.dylib`
//! - Linux : `libonnxruntime.so`
//! - Windows: `onnxruntime.dll`
//!
//! # Privacy
//! Raw audio bytes are held in memory only for the duration of feature
//! extraction and are never written to disk by this adapter.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use ndarray::{Array1, Array2, Array3};
use ort::{inputs, session::Session, value::Tensor};
use std::sync::Mutex;
use pond_core::domain::biometric::SpeakerEmbedding;
use pond_core::ports::speaker_id::SpeakerIdentification;
use rustfft::{num_complex::Complex, FftPlanner};
use sqlx::{Pool, Sqlite};
use std::f32::consts::PI;
use std::path::Path;
use tracing::{debug, info, warn};
use uuid::Uuid;

// ── Model constants ────────────────────────────────────────────────────────────

const MODEL_NAME: &str = "x-vector";
const DIMS: u32 = 512;
const DEFAULT_THRESHOLD: f32 = 0.55;

// ── Audio feature-extraction constants ────────────────────────────────────────

const TARGET_SAMPLE_RATE: u32 = 16_000;
const FRAME_LEN: usize = 400;   // 25 ms at 16 kHz
const FRAME_SHIFT: usize = 160; // 10 ms at 16 kHz
const N_FFT: usize = 512;
const N_MELS: usize = 24;
const F_MIN: f32 = 20.0;
const F_MAX: f32 = 8_000.0;
const PRE_EMPHASIS: f32 = 0.97;

// ── Adapter ────────────────────────────────────────────────────────────────────

/// Speaker identification adapter backed by an ONNX x-vector model.
///
/// Takes both SQLite pools because:
/// - `system_pool` → `speaker_embeddings` table (permanent enrollment data)
/// - `logs_pool`   → `diarization_logs` + `biometric_audit_log` (time-series events)
pub struct OnnxSpeakerAdapter {
    session: Mutex<Session>,
    system_pool: Pool<Sqlite>,
    logs_pool: Pool<Sqlite>,
    threshold: f32,
    input_name: String,
    output_name: String,
}

impl OnnxSpeakerAdapter {
    /// Load the ONNX model from `model_path` and connect to the SQLite pools.
    ///
    /// Returns an error if the model file is missing or malformed — callers
    /// should handle this gracefully (e.g. disable speaker ID rather than crash).
    pub fn new(
        model_path: impl AsRef<Path>,
        system_pool: Pool<Sqlite>,
        logs_pool: Pool<Sqlite>,
    ) -> Result<Self> {
        let session = Session::builder()
            .context("ONNX Runtime initialisation failed")?
            .commit_from_file(model_path.as_ref())
            .with_context(|| format!(
                "Failed to load speaker model from {:?}",
                model_path.as_ref()
            ))?;

        // Read tensor names directly from the model so the adapter works with
        // any ONNX export, not just the SpeechBrain naming convention.
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "feats".to_string());
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| "embedding".to_string());

        Ok(Self {
            session: Mutex::new(session),
            system_pool,
            logs_pool,
            threshold: DEFAULT_THRESHOLD,
            input_name,
            output_name,
        })
    }

    /// Override the cosine-similarity threshold (default 0.55).
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }

    /// Override the ONNX input/output tensor names.
    ///
    /// Defaults match the SpeechBrain x-vector ONNX export (`"feats"` / `"embedding"`).
    /// Use this when targeting a different exported model.
    pub fn with_tensor_names(
        mut self,
        input: impl Into<String>,
        output: impl Into<String>,
    ) -> Self {
        self.input_name = input.into();
        self.output_name = output.into();
        self
    }

    // ── Private helpers ────────────────────────────────────────────────────────

    async fn extract_embedding(&self, audio_bytes: &[u8]) -> Result<Vec<f32>> {
        let samples = decode_wav(audio_bytes)?;
        let features = compute_log_mel_filterbank(&samples)?; // Array2 [T, N_MELS]

        let t = features.nrows();
        let (raw, _) = features.into_raw_vec_and_offset();
        let feats = Array3::<f32>::from_shape_vec((1, t, N_MELS), raw)
            .context("Failed to reshape features into 3-D tensor")?;

        let tensor = Tensor::<f32>::from_array(feats).context("Failed to create input tensor")?;
        let mut guard = self.session.lock().expect("session mutex poisoned");
        let outputs = guard
            .run(inputs![self.input_name.as_str() => tensor])
            .context("ONNX inference failed")?;

        // try_extract_tensor returns (&Shape, &[T]) in ort 2.0-rc.12
        let (_, flat_data) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .context("Failed to extract embedding tensor")?;

        let embedding: Vec<f32> = flat_data.to_vec();

        if embedding.len() != DIMS as usize {
            return Err(anyhow!(
                "Model produced {}-d embedding, expected {}-d — check tensor names and model",
                embedding.len(),
                DIMS
            ));
        }

        Ok(embedding)
    }

    async fn log_identification(
        &self,
        session_id: Option<&str>,
        profile_id: Option<&str>,
        confidence: Option<f32>,
    ) {
        if let Err(e) = sqlx::query(
            "INSERT INTO diarization_logs (session_id, profile_id, confidence, model) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(profile_id)
        .bind(confidence)
        .bind(MODEL_NAME)
        .execute(&self.logs_pool)
        .await
        {
            warn!("Failed to write diarization log: {}", e);
        }

        if let Err(e) = sqlx::query(
            "INSERT INTO biometric_audit_log \
             (profile_id, action, modality, confidence, model) \
             VALUES (?, 'identify_speaker', 'voice', ?, ?)",
        )
        .bind(profile_id)
        .bind(confidence)
        .bind(MODEL_NAME)
        .execute(&self.logs_pool)
        .await
        {
            warn!("Failed to write biometric audit log: {}", e);
        }
    }

    async fn log_enrollment(&self, profile_id: &str) {
        if let Err(e) = sqlx::query(
            "INSERT INTO biometric_audit_log (profile_id, action, modality, model) \
             VALUES (?, 'enroll_speaker', 'voice', ?)",
        )
        .bind(profile_id)
        .bind(MODEL_NAME)
        .execute(&self.logs_pool)
        .await
        {
            warn!("Failed to write enrolment audit log: {}", e);
        }
    }
}

// ── SpeakerIdentification impl ─────────────────────────────────────────────────

#[async_trait]
impl SpeakerIdentification for OnnxSpeakerAdapter {
    async fn register_speaker(
        &self,
        profile_id: &str,
        audio_bytes: &[u8],
    ) -> Result<SpeakerEmbedding> {
        let embedding = self.extract_embedding(audio_bytes).await?;
        let blob = encode_embedding(&embedding);

        let id = Uuid::new_v4().to_string();
        let now_str = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        sqlx::query(
            "INSERT INTO speaker_embeddings \
             (id, profile_id, model, dims, threshold, embedding, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(profile_id)
        .bind(MODEL_NAME)
        .bind(DIMS as i64)
        .bind(self.threshold)
        .bind(&blob)
        .bind(&now_str)
        .execute(&self.system_pool)
        .await
        .context("Failed to insert speaker embedding")?;

        info!(profile_id, embedding_id = %id, "Speaker enrolled");
        self.log_enrollment(profile_id).await;

        Ok(SpeakerEmbedding {
            id,
            profile_id: profile_id.to_string(),
            model: MODEL_NAME.to_string(),
            dims: DIMS,
            threshold: self.threshold,
            created_at: chrono::Utc::now(),
        })
    }

    async fn identify_speaker(&self, audio_bytes: &[u8]) -> Result<Option<(String, f32)>> {
        let query_embedding = self.extract_embedding(audio_bytes).await?;

        let rows: Vec<(String, Vec<u8>)> =
            sqlx::query_as("SELECT profile_id, embedding FROM speaker_embeddings")
                .fetch_all(&self.system_pool)
                .await
                .context("Failed to load speaker embeddings")?;

        if rows.is_empty() {
            debug!("No enrolled speakers — session proceeds without attribution");
            self.log_identification(None, None, None).await;
            return Ok(None);
        }

        let mut best_profile: Option<String> = None;
        let mut best_score: f32 = 0.0;

        for (profile_id, blob) in &rows {
            let stored = match decode_embedding(blob) {
                Ok(v) => v,
                Err(e) => {
                    warn!(profile_id, "Skipping malformed embedding: {}", e);
                    continue;
                }
            };
            let score = cosine_similarity(&query_embedding, &stored);
            if score > best_score {
                best_score = score;
                best_profile = Some(profile_id.clone());
            }
        }

        if best_score >= self.threshold {
            let pid = best_profile.unwrap();
            debug!(profile_id = %pid, confidence = best_score, "Speaker identified");
            self.log_identification(None, Some(&pid), Some(best_score)).await;
            Ok(Some((pid, best_score)))
        } else {
            debug!(best_score, threshold = self.threshold, "Speaker below threshold — unknown");
            self.log_identification(None, None, Some(best_score)).await;
            Ok(None)
        }
    }

    async fn enrollment_count(&self, profile_id: &str) -> Result<u32> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM speaker_embeddings WHERE profile_id = ?",
        )
        .bind(profile_id)
        .fetch_one(&self.system_pool)
        .await
        .context("Failed to count speaker embeddings")?;
        Ok(count as u32)
    }

    async fn delete_speaker(&self, profile_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM speaker_embeddings WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.system_pool)
            .await
            .context("Failed to delete speaker embeddings")?;

        if let Err(e) = sqlx::query(
            "INSERT INTO biometric_audit_log \
             (profile_id, action, modality, model) \
             VALUES (?, 'delete_biometrics', 'voice', ?)",
        )
        .bind(profile_id)
        .bind(MODEL_NAME)
        .execute(&self.logs_pool)
        .await
        {
            warn!("Failed to write delete audit log: {}", e);
        }

        info!(profile_id, "Speaker embeddings deleted");
        Ok(())
    }
}

// ── Audio feature extraction ───────────────────────────────────────────────────

/// Decode WAV bytes to normalised f32 mono samples.
///
/// Requires 16 kHz input; stereo is mixed down to mono.
/// Supports 16-bit int, 24-bit int, 32-bit int, and 32-bit float PCM.
fn decode_wav(bytes: &[u8]) -> Result<Vec<f32>> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = hound::WavReader::new(cursor).context("Invalid WAV data")?;
    let spec = reader.spec();

    if spec.sample_rate != TARGET_SAMPLE_RATE {
        return Err(anyhow!(
            "Expected {}Hz audio, got {}Hz — resample before passing to speaker ID",
            TARGET_SAMPLE_RATE,
            spec.sample_rate
        ));
    }

    let channels = spec.channels as usize;

    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to read f32 samples")?,
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32_768.0))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to read i16 samples")?,
        (hound::SampleFormat::Int, 24) => reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / 8_388_608.0))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to read 24-bit samples")?,
        (hound::SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / 2_147_483_648.0))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to read i32 samples")?,
        _ => return Err(anyhow!(
            "Unsupported WAV format: {:?} {}-bit (expected 16/24/32-bit int or 32-bit float)",
            spec.sample_format,
            spec.bits_per_sample
        )),
    };

    Ok(if channels == 1 {
        samples
    } else {
        samples
            .chunks(channels)
            .map(|ch| ch.iter().sum::<f32>() / channels as f32)
            .collect()
    })
}

/// Build a mel triangular filterbank matrix of shape `[n_mels, n_fft/2+1]`.
fn build_mel_filterbank(
    sr: u32,
    n_fft: usize,
    n_mels: usize,
    fmin: f32,
    fmax: f32,
) -> Array2<f32> {
    let hz_to_mel = |hz: f32| 2595.0_f32 * (1.0 + hz / 700.0).log10();
    let mel_to_hz = |mel: f32| 700.0_f32 * (10_f32.powf(mel / 2595.0) - 1.0);

    let mel_min = hz_to_mel(fmin);
    let mel_max = hz_to_mel(fmax);
    let freq_bins = n_fft / 2 + 1;
    let hz_per_bin = sr as f32 / n_fft as f32;

    let bin_points: Vec<f32> = (0..=(n_mels + 1))
        .map(|i| {
            let mel = mel_min + (mel_max - mel_min) * i as f32 / (n_mels + 1) as f32;
            mel_to_hz(mel) / hz_per_bin
        })
        .collect();

    let mut fb = Array2::<f32>::zeros((n_mels, freq_bins));
    for m in 0..n_mels {
        let (left, center, right) = (bin_points[m], bin_points[m + 1], bin_points[m + 2]);
        for k in 0..freq_bins {
            let k_f = k as f32;
            if k_f >= left && k_f <= center {
                fb[[m, k]] = (k_f - left) / (center - left).max(1e-10);
            } else if k_f > center && k_f <= right {
                fb[[m, k]] = (right - k_f) / (right - center).max(1e-10);
            }
        }
    }
    fb
}

/// Compute per-utterance CMVN-normalised 40-dim log-mel filterbank features.
///
/// Returns `Array2<f32>` of shape `[n_frames, N_MELS]`.
fn compute_log_mel_filterbank(samples: &[f32]) -> Result<Array2<f32>> {
    if samples.len() < FRAME_LEN {
        return Err(anyhow!(
            "Audio too short: {} samples, need at least {} ({}ms at {}Hz)",
            samples.len(),
            FRAME_LEN,
            FRAME_LEN * 1000 / TARGET_SAMPLE_RATE as usize,
            TARGET_SAMPLE_RATE
        ));
    }

    // Pre-emphasis
    let mut emphasized = Vec::with_capacity(samples.len());
    emphasized.push(samples[0]);
    for i in 1..samples.len() {
        emphasized.push(samples[i] - PRE_EMPHASIS * samples[i - 1]);
    }

    // Hamming window
    let window: Vec<f32> = (0..FRAME_LEN)
        .map(|i| 0.54 - 0.46 * (2.0 * PI * i as f32 / (FRAME_LEN - 1) as f32).cos())
        .collect();

    let filterbank = build_mel_filterbank(TARGET_SAMPLE_RATE, N_FFT, N_MELS, F_MIN, F_MAX);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let freq_bins = N_FFT / 2 + 1;

    let n_frames = (emphasized.len() - FRAME_LEN) / FRAME_SHIFT + 1;
    let mut features = Array2::<f32>::zeros((n_frames, N_MELS));
    let mut frame_buf = vec![Complex::new(0.0_f32, 0.0); N_FFT];

    for f in 0..n_frames {
        let start = f * FRAME_SHIFT;
        for i in 0..N_FFT {
            frame_buf[i] = if i < FRAME_LEN {
                Complex::new(emphasized[start + i] * window[i], 0.0)
            } else {
                Complex::new(0.0, 0.0)
            };
        }
        fft.process(&mut frame_buf);

        let power: Vec<f32> = frame_buf[..freq_bins].iter().map(|c| c.norm_sqr()).collect();

        for m in 0..N_MELS {
            let energy: f32 = (0..freq_bins).map(|k| filterbank[[m, k]] * power[k]).sum();
            features[[f, m]] = energy.max(1e-10).ln();
        }
    }

    // Per-utterance CMVN
    let n = n_frames as f32;
    let mut mean = Array1::<f32>::zeros(N_MELS);
    let mut sq_mean = Array1::<f32>::zeros(N_MELS);
    for row in features.rows() {
        mean += &row;
        sq_mean += &row.mapv(|x| x * x);
    }
    mean /= n;
    sq_mean /= n;
    let std_dev = (sq_mean - mean.mapv(|x| x * x)).mapv(|x| x.max(0.0).sqrt() + 1e-8);

    for mut row in features.rows_mut() {
        row -= &mean;
        row /= &std_dev;
    }

    Ok(features)
}

// ── Encoding helpers ───────────────────────────────────────────────────────────

fn encode_embedding(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn decode_embedding(blob: &[u8]) -> Result<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return Err(anyhow!(
            "Embedding BLOB length {} is not a multiple of 4",
            blob.len()
        ));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sine_wav(freq_hz: f32, duration_ms: u32) -> Vec<u8> {
        let n_samples = (TARGET_SAMPLE_RATE * duration_ms / 1000) as usize;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf = Vec::new();
        let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
        for i in 0..n_samples {
            let t = i as f32 / TARGET_SAMPLE_RATE as f32;
            let s = (0.5 * (2.0 * PI * freq_hz * t).sin() * i16::MAX as f32) as i16;
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();
        buf
    }

    #[test]
    fn decode_wav_produces_normalised_samples() {
        let wav = make_sine_wav(440.0, 500);
        let samples = decode_wav(&wav).unwrap();
        assert!(!samples.is_empty());
        for s in &samples {
            assert!(*s >= -1.0 && *s <= 1.0, "sample {s} out of [-1, 1]");
        }
    }

    #[test]
    fn decode_wav_rejects_wrong_sample_rate() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf = Vec::new();
        let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
        writer.write_sample(0_i16).unwrap();
        writer.finalize().unwrap();

        let err = decode_wav(&buf).unwrap_err();
        assert!(err.to_string().contains("44100"));
    }

    #[test]
    fn log_mel_features_have_correct_shape() {
        let wav = make_sine_wav(220.0, 1000);
        let samples = decode_wav(&wav).unwrap();
        let features = compute_log_mel_filterbank(&samples).unwrap();

        assert_eq!(features.ncols(), N_MELS);
        // 1 s at 16 kHz: (16000 - 400) / 160 + 1 = 98 frames
        assert!(features.nrows() >= 95 && features.nrows() <= 105,
            "unexpected frame count: {}", features.nrows());
    }

    #[test]
    fn log_mel_rejects_short_audio() {
        let too_short = vec![0.0f32; FRAME_LEN - 1];
        let err = compute_log_mel_filterbank(&too_short).unwrap_err();
        assert!(err.to_string().contains("too short"));
    }

    #[test]
    fn adapter_fails_gracefully_on_missing_model() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (system, logs) = rt.block_on(async {
            use sqlx::sqlite::SqlitePoolOptions;
            let s = SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
            let l = SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
            (s, l)
        });
        let result = OnnxSpeakerAdapter::new("/nonexistent/speaker.onnx", system, logs);
        assert!(result.is_err(), "Should fail when model file is missing");
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0f32, 0.5, 0.3];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-5);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let original: Vec<f32> = (0..512).map(|i| i as f32 * 0.001).collect();
        let blob = encode_embedding(&original);
        let recovered = decode_embedding(&blob).unwrap();
        assert_eq!(original.len(), recovered.len());
        for (a, b) in original.iter().zip(recovered.iter()) {
            assert!((a - b).abs() < 1e-7, "roundtrip mismatch: {a} vs {b}");
        }
    }
}
