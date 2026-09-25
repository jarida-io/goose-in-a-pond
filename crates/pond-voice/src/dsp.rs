//! Pure, `std`-only audio primitives shared by the server and both adapters.
//!
//! TypeScript counterparts live in `pond-desktop/src/modes/voice/webAudioUtils.ts`.

// ── Level ────────────────────────────────────────────────────────────────────

/// Root-mean-square level of normalised samples; zero for an empty block.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

// ── Rate conversion ──────────────────────────────────────────────────────────

/// Linearly resample to 16 kHz, the rate whisper expects.
pub fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == 16_000 || src_rate == 0 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = 16_000.0_f64 / src_rate as f64;
    let new_len = (samples.len() as f64 * ratio) as usize;
    (0..new_len)
        .map(|i| {
            let src = i as f64 / ratio;
            let lo = src.floor() as usize;
            let hi = (lo + 1).min(samples.len().saturating_sub(1));
            let frac = (src - src.floor()) as f32;
            samples[lo] * (1.0 - frac) + samples[hi] * frac
        })
        .collect()
}

// ── Format conversion ────────────────────────────────────────────────────────

/// Convert normalised f32 samples to little-endian 16-bit PCM.
pub fn f32_to_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Wrap little-endian 16-bit mono PCM in a canonical 44-byte WAV header.
pub fn encode_wav_pcm16(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    const CHANNELS: u16 = 1;
    const BITS: u16 = 16;
    let byte_rate = sample_rate * CHANNELS as u32 * BITS as u32 / 8;
    let block_align = CHANNELS * BITS / 8;
    let data_len = pcm.len() as u32;

    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&BITS.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

pub fn encode_wav_mono_16k(samples: &[f32]) -> Vec<u8> {
    encode_wav_pcm16(&f32_to_pcm16(samples), 16_000)
}

// ── Decoding ─────────────────────────────────────────────────────────────────

/// Why a WAV could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WavError {
    /// Not a RIFF/WAVE container at all.
    NotRiff,
    /// Structurally a WAV but a required chunk is missing.
    MissingChunk(&'static str),
    /// Truncated, or a chunk header claims more bytes than exist.
    Truncated,
    /// A shape this decoder does not handle, described for the caller.
    Unsupported(String),
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRiff => write!(f, "not a RIFF/WAVE file"),
            Self::MissingChunk(c) => write!(f, "WAV is missing its '{c}' chunk"),
            Self::Truncated => write!(f, "WAV data is truncated"),
            Self::Unsupported(d) => write!(f, "unsupported WAV format: {d}"),
        }
    }
}

impl std::error::Error for WavError {}

/// Decoded mono audio.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedWav {
    /// Normalised mono samples in `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

const FMT_PCM: u16 = 1;
const FMT_IEEE_FLOAT: u16 = 3;
const FMT_EXTENSIBLE: u16 = 0xFFFE;

/// Decode a WAV to normalised mono f32, walking the RIFF chunk list.
///
/// Untrusted network input: must never panic, and allocations are bounded by bytes present.
pub fn decode_wav(bytes: &[u8]) -> Result<DecodedWav, WavError> {
    // RIFF....WAVE
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotRiff);
    }

    let mut pos = 12usize;
    let mut fmt: Option<FmtChunk> = None;
    let mut data: Option<&[u8]> = None;

    // Chunks: 4-byte id, 4-byte LE size, payload, then a pad byte if the size is odd.
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body_start = pos + 8;
        // Clamp: a truncated file or hostile size must not slice past the buffer.
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];

        match id {
            b"fmt " => fmt = Some(parse_fmt(body)?),
            b"data" => data = Some(body),
            _ => {} // LIST, INFO, JUNK, fact, id3 … skipped by design
        }

        // saturating_add stops a hostile size wrapping the cursor back into an endless loop.
        let advance = size + (size & 1);
        pos = body_start.saturating_add(advance);
        if advance == 0 && id != b"data" {
            // body_start is already 8 past pos, so zero-length chunks still make progress.
            continue;
        }
    }

    let fmt = fmt.ok_or(WavError::MissingChunk("fmt "))?;
    let data = data.ok_or(WavError::MissingChunk("data"))?;
    if data.is_empty() {
        return Ok(DecodedWav {
            samples: Vec::new(),
            sample_rate: fmt.sample_rate,
        });
    }

    let samples = decode_samples(data, &fmt)?;
    Ok(DecodedWav {
        samples,
        sample_rate: fmt.sample_rate,
    })
}

struct FmtChunk {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

fn parse_fmt(body: &[u8]) -> Result<FmtChunk, WavError> {
    // 16 bytes is the PCM minimum; 18 adds cbSize, 40 is EXTENSIBLE.
    if body.len() < 16 {
        return Err(WavError::Truncated);
    }
    let mut format_tag = u16::from_le_bytes([body[0], body[1]]);
    let channels = u16::from_le_bytes([body[2], body[3]]);
    let sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let bits_per_sample = u16::from_le_bytes([body[14], body[15]]);

    // EXTENSIBLE's real format is the first two bytes of its SubFormat GUID, at offset 24.
    if format_tag == FMT_EXTENSIBLE {
        if body.len() < 26 {
            return Err(WavError::Unsupported(
                "WAVE_FORMAT_EXTENSIBLE without a SubFormat GUID".into(),
            ));
        }
        format_tag = u16::from_le_bytes([body[24], body[25]]);
    }

    if channels == 0 {
        return Err(WavError::Unsupported("zero channels".into()));
    }
    if sample_rate == 0 {
        return Err(WavError::Unsupported("zero sample rate".into()));
    }

    Ok(FmtChunk {
        format_tag,
        channels,
        sample_rate,
        bits_per_sample,
    })
}

fn decode_samples(data: &[u8], fmt: &FmtChunk) -> Result<Vec<f32>, WavError> {
    let channels = fmt.channels as usize;

    // Per-sample decoders, all normalising into [-1.0, 1.0].
    let (bytes_per_sample, convert): (usize, fn(&[u8]) -> f32) =
        match (fmt.format_tag, fmt.bits_per_sample) {
            // 8-bit PCM is unsigned around 128, the only depth that isn't two's complement.
            (FMT_PCM, 8) => (1, |b| (b[0] as f32 - 128.0) / 128.0),
            (FMT_PCM, 16) => (2, |b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0),
            (FMT_PCM, 24) => (3, |b| {
                // Sign-extend 24-bit little-endian into i32.
                let v = ((b[2] as i32) << 24 | (b[1] as i32) << 16 | (b[0] as i32) << 8) >> 8;
                v as f32 / 8_388_608.0
            }),
            (FMT_PCM, 32) => (4, |b| {
                i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0
            }),
            (FMT_IEEE_FLOAT, 32) => (4, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            (FMT_IEEE_FLOAT, 64) => (8, |b| {
                f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32
            }),
            (tag, bits) => {
                return Err(WavError::Unsupported(format!(
                    "format tag {tag} at {bits} bits per sample"
                )))
            }
        };

    let frame_bytes = bytes_per_sample * channels;
    if frame_bytes == 0 {
        return Err(WavError::Unsupported("zero-size frame".into()));
    }

    let frames = data.len() / frame_bytes;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let base = f * frame_bytes;
        // Downmix by averaging; whisper wants mono.
        let mut acc = 0.0f32;
        for c in 0..channels {
            let s = base + c * bytes_per_sample;
            acc += convert(&data[s..s + bytes_per_sample]);
        }
        out.push(acc / channels as f32);
    }
    Ok(out)
}

// ── Silence ──────────────────────────────────────────────────────────────────

/// What one poll of the VAD implies for a speculative transcription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    None,
    /// A silence run began: transcribe speculatively during the rest of the silence wait.
    SpawnSpeculative,
    /// Speech resumed, so any in-flight speculative result is stale.
    DiscardSpeculative,
    /// Silence held long enough: end of utterance.
    Confirmed,
}

/// Whether one frame of audio is speech: the evidence [`SpeculativeVad`] runs on.
///
/// Frames come at whatever size the caller polls; fixed-window detectors buffer internally.
pub trait SpeechDetector {
    /// Is this frame speech? Takes `&mut self` as detectors (Silero's LSTM) keep state.
    fn is_speech(&mut self, frame: &[f32]) -> bool;

    /// Forget all state; called between utterances so one turn can't bias the next.
    fn reset(&mut self) {}
}

/// Re-frames variable-length reads into exactly consecutive fixed-size windows.
///
/// Models need exact windows (Silero: 512 samples at 16 kHz), but capture reads jitter.
#[derive(Debug, Clone)]
pub struct Windower {
    size: usize,
    buf: Vec<f32>,
}

impl Windower {
    pub fn new(size: usize) -> Self {
        Self {
            size,
            buf: Vec::with_capacity(size * 2),
        }
    }

    /// Append `samples` and pass each complete window (zero or several) to `on_window`.
    pub fn push(&mut self, samples: &[f32], mut on_window: impl FnMut(&[f32])) {
        self.buf.extend_from_slice(samples);
        let mut consumed = 0;
        while self.buf.len() - consumed >= self.size {
            on_window(&self.buf[consumed..consumed + self.size]);
            consumed += self.size;
        }
        if consumed > 0 {
            // The leftover is under one window, so draining the front copies little.
            self.buf.drain(..consumed);
        }
    }

    /// Drop the leftover. The next window starts clean.
    pub fn reset(&mut self) {
        self.buf.clear();
    }

    /// Samples held back, waiting for a full window.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// Speech is RMS at or above a threshold. The only detector that needs no model file.
#[derive(Debug, Clone, Copy)]
pub struct RmsDetector {
    threshold: f32,
}

impl RmsDetector {
    pub fn new(threshold: f32) -> Self {
        Self { threshold }
    }
}

impl SpeechDetector for RmsDetector {
    fn is_speech(&mut self, frame: &[f32]) -> bool {
        // `>=`: a frame at the threshold is speech, and NaN is silence (as in the whisper onset
        // gate); `!(rms < t)` would let a NaN stream hold the endpoint open forever.
        rms(frame) >= self.threshold
    }
}

/// Debounced end-of-speech detector that also drives speculative inference.
///
/// Feed one speech decision per `poll_ms`; the start of each silence run is reported once.
#[derive(Debug, Clone)]
pub struct SpeculativeVad {
    silent_for_ms: u64,
    silence_ms: u64,
    poll_ms: u64,
}

impl SpeculativeVad {
    pub fn new(silence_ms: u64, poll_ms: u64) -> Self {
        Self {
            silent_for_ms: 0,
            silence_ms,
            poll_ms,
        }
    }

    /// Advance one poll.
    pub fn on_speech(&mut self, is_speech: bool) -> VadEvent {
        if !is_speech {
            let was_speaking = self.silent_for_ms == 0;
            self.silent_for_ms += self.poll_ms;
            if self.silent_for_ms >= self.silence_ms {
                VadEvent::Confirmed
            } else if was_speaking {
                VadEvent::SpawnSpeculative
            } else {
                VadEvent::None
            }
        } else {
            let was_silent = self.silent_for_ms != 0;
            self.silent_for_ms = 0;
            if was_silent {
                VadEvent::DiscardSpeculative
            } else {
                VadEvent::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── round trip ───────────────────────────────────────────────────────

    #[test]
    fn encode_then_decode_round_trips() {
        let src: Vec<f32> = (0..800).map(|i| (i as f32 / 40.0).sin() * 0.7).collect();
        let wav = encode_wav_mono_16k(&src);
        let got = decode_wav(&wav).expect("our own encoder must decode");
        assert_eq!(got.sample_rate, 16_000);
        assert_eq!(got.samples.len(), src.len());
        for (a, b) in src.iter().zip(&got.samples) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn scaling_matches_the_two_implementations_this_replaces() {
        assert_eq!(f32_to_pcm16(&[1.0]), 32_767i16.to_le_bytes());
        assert_eq!(f32_to_pcm16(&[-1.0]), (-32_767i16).to_le_bytes());
        assert_eq!(f32_to_pcm16(&[0.0]), 0i16.to_le_bytes());
        // Out-of-range input clamps rather than wrapping.
        assert_eq!(f32_to_pcm16(&[9.0]), 32_767i16.to_le_bytes());
        assert_eq!(f32_to_pcm16(&[-9.0]), (-32_767i16).to_le_bytes());
    }

    #[test]
    fn the_header_is_the_canonical_44_bytes() {
        let wav = encode_wav_pcm16(&[0, 0, 0, 0], 22_050);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 48);
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 22_050);
    }

    // ── the shapes a real phone emits ────────────────────────────────────

    /// Builds a WAV with arbitrary extra chunks before `data`.
    fn wav_with(fmt_body: Vec<u8>, extra_chunks: &[(&[u8; 4], Vec<u8>)], data: Vec<u8>) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&(fmt_body.len() as u32).to_le_bytes());
        body.extend_from_slice(&fmt_body);
        if fmt_body.len() % 2 == 1 {
            body.push(0);
        }
        for (id, payload) in extra_chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                body.push(0);
            }
        }
        body.extend_from_slice(b"data");
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&data);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn pcm_fmt(channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let block_align = channels * bits / 8;
        let byte_rate = rate * block_align as u32;
        let mut f = Vec::new();
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&channels.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&byte_rate.to_le_bytes());
        f.extend_from_slice(&block_align.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        f
    }

    #[test]
    fn a_list_chunk_before_data_no_longer_shifts_the_samples() {
        let pcm = f32_to_pcm16(&[0.5, -0.5, 0.25, -0.25]);
        let wav = wav_with(
            pcm_fmt(1, 16_000, 16),
            &[(b"LIST", b"INFOISFT\0Recorder".to_vec())],
            pcm,
        );
        let got = decode_wav(&wav).unwrap();
        assert_eq!(got.samples.len(), 4);
        assert!((got.samples[0] - 0.5).abs() < 1e-3, "{:?}", got.samples);
        assert!((got.samples[1] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn an_18_byte_fmt_chunk_is_accepted() {
        let mut fmt = pcm_fmt(1, 16_000, 16);
        fmt.extend_from_slice(&0u16.to_le_bytes()); // cbSize
        let wav = wav_with(fmt, &[], f32_to_pcm16(&[0.5]));
        assert_eq!(decode_wav(&wav).unwrap().samples.len(), 1);
    }

    #[test]
    fn wave_format_extensible_resolves_through_its_subformat() {
        let mut fmt = pcm_fmt(1, 48_000, 16);
        fmt[0..2].copy_from_slice(&FMT_EXTENSIBLE.to_le_bytes());
        fmt.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        fmt.extend_from_slice(&16u16.to_le_bytes()); // valid bits
        fmt.extend_from_slice(&3u32.to_le_bytes()); // channel mask
        fmt.extend_from_slice(&FMT_PCM.to_le_bytes()); // SubFormat GUID head
        fmt.extend_from_slice(&[0u8; 14]);
        let wav = wav_with(fmt, &[], f32_to_pcm16(&[0.5, -0.5]));
        let got = decode_wav(&wav).unwrap();
        assert_eq!(got.sample_rate, 48_000);
        assert_eq!(got.samples.len(), 2);
    }

    #[test]
    fn stereo_is_downmixed_by_averaging_not_truncated() {
        // L = +0.5, R = -0.5 -> mono 0.0
        let mut pcm = Vec::new();
        pcm.extend_from_slice(&f32_to_pcm16(&[0.5]));
        pcm.extend_from_slice(&f32_to_pcm16(&[-0.5]));
        let wav = wav_with(pcm_fmt(2, 16_000, 16), &[], pcm);
        let got = decode_wav(&wav).unwrap();
        assert_eq!(got.samples.len(), 1, "one frame, not two");
        assert!(got.samples[0].abs() < 1e-3, "got {}", got.samples[0]);
    }

    #[test]
    fn twenty_four_bit_pcm_sign_extends() {
        // -0.5 and +0.5 as 24-bit LE.
        let mut pcm = Vec::new();
        pcm.extend_from_slice(&[0x00, 0x00, 0xC0]); // -4194304 / 8388608 = -0.5
        pcm.extend_from_slice(&[0x00, 0x00, 0x40]); // +4194304 / 8388608 = +0.5
        let wav = wav_with(pcm_fmt(1, 16_000, 24), &[], pcm);
        let got = decode_wav(&wav).unwrap();
        assert!((got.samples[0] + 0.5).abs() < 1e-3, "{:?}", got.samples);
        assert!((got.samples[1] - 0.5).abs() < 1e-3, "{:?}", got.samples);
    }

    #[test]
    fn eight_bit_pcm_is_unsigned_around_128() {
        let wav = wav_with(pcm_fmt(1, 8_000, 8), &[], vec![128, 255, 0]);
        let got = decode_wav(&wav).unwrap();
        assert!(got.samples[0].abs() < 1e-6, "128 is the midpoint");
        assert!(got.samples[1] > 0.9);
        assert!(got.samples[2] < -0.9);
    }

    #[test]
    fn ieee_float_32_is_read_directly() {
        let mut fmt = pcm_fmt(1, 44_100, 32);
        fmt[0..2].copy_from_slice(&FMT_IEEE_FLOAT.to_le_bytes());
        let mut pcm = Vec::new();
        pcm.extend_from_slice(&0.25f32.to_le_bytes());
        pcm.extend_from_slice(&(-0.75f32).to_le_bytes());
        let wav = wav_with(fmt, &[], pcm);
        let got = decode_wav(&wav).unwrap();
        assert!((got.samples[0] - 0.25).abs() < 1e-6);
        assert!((got.samples[1] + 0.75).abs() < 1e-6);
    }

    #[test]
    fn an_odd_sized_chunk_pad_byte_is_honoured() {
        let pcm = f32_to_pcm16(&[0.5, -0.5]);
        // 3-byte payload forces a pad byte before `data`.
        let wav = wav_with(pcm_fmt(1, 16_000, 16), &[(b"JUNK", vec![1, 2, 3])], pcm);
        assert_eq!(decode_wav(&wav).unwrap().samples.len(), 2);
    }

    // ── hostile and malformed input ──────────────────────────────────────

    #[test]
    fn non_riff_input_is_rejected_not_guessed_at() {
        assert_eq!(decode_wav(b"").unwrap_err(), WavError::NotRiff);
        assert_eq!(
            decode_wav(b"not a wav file").unwrap_err(),
            WavError::NotRiff
        );
        // An MP3 frame header, which a phone might well send.
        assert_eq!(
            decode_wav(&[0xFF, 0xFB, 0x90, 0x00]).unwrap_err(),
            WavError::NotRiff
        );
    }

    #[test]
    fn riff_without_a_fmt_chunk_reports_the_missing_chunk() {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        assert_eq!(
            decode_wav(&out).unwrap_err(),
            WavError::MissingChunk("fmt ")
        );
    }

    #[test]
    fn a_lying_chunk_size_cannot_read_past_the_buffer() {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&u32::MAX.to_le_bytes()); // claims 4 GiB
        out.extend_from_slice(&pcm_fmt(1, 16_000, 16));
        let err = decode_wav(&out).unwrap_err();
        assert!(matches!(err, WavError::MissingChunk(_)), "{err:?}");
    }

    #[test]
    fn a_truncated_fmt_chunk_is_an_error_not_a_panic() {
        let wav = wav_with(vec![1, 0, 1, 0], &[], vec![]);
        assert_eq!(decode_wav(&wav).unwrap_err(), WavError::Truncated);
    }

    #[test]
    fn zero_channels_or_rate_is_rejected() {
        let z = wav_with(pcm_fmt(0, 16_000, 16), &[], vec![0, 0]);
        assert!(matches!(decode_wav(&z), Err(WavError::Unsupported(_))));
        let r = wav_with(pcm_fmt(1, 0, 16), &[], vec![0, 0]);
        assert!(matches!(decode_wav(&r), Err(WavError::Unsupported(_))));
    }

    #[test]
    fn an_unsupported_depth_names_itself_rather_than_returning_noise() {
        let wav = wav_with(pcm_fmt(1, 16_000, 12), &[], vec![0, 0, 0]);
        match decode_wav(&wav) {
            Err(WavError::Unsupported(d)) => assert!(d.contains("12"), "{d}"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn a_trailing_partial_frame_is_dropped_not_read_past() {
        // 5 bytes of 16-bit stereo = one whole frame (4 bytes) + 1 stray.
        let wav = wav_with(pcm_fmt(2, 16_000, 16), &[], vec![0, 0, 0, 0, 7]);
        assert_eq!(decode_wav(&wav).unwrap().samples.len(), 1);
    }

    #[test]
    fn an_empty_data_chunk_yields_no_samples_rather_than_an_error() {
        let wav = wav_with(pcm_fmt(1, 16_000, 16), &[], vec![]);
        let got = decode_wav(&wav).unwrap();
        assert!(got.samples.is_empty());
        assert_eq!(got.sample_rate, 16_000);
    }

    #[test]
    fn no_prefix_of_a_valid_wav_can_panic() {
        let full = wav_with(
            pcm_fmt(2, 44_100, 24),
            &[(b"LIST", b"INFO".to_vec()), (b"JUNK", vec![9; 7])],
            vec![1; 60],
        );
        for n in 0..full.len() {
            let _ = decode_wav(&full[..n]); // must not panic
        }
    }

    // ── level, rate, silence ─────────────────────────────────────────────

    #[test]
    fn rms_of_silence_is_zero_and_of_full_scale_is_one() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0; 16]), 0.0);
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn resampling_is_identity_at_the_target_rate() {
        let s = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_to_16k(&s, 16_000), s);
        // Degenerate inputs must not divide by zero or panic.
        assert_eq!(resample_to_16k(&s, 0), s);
        assert!(resample_to_16k(&[], 44_100).is_empty());
    }

    #[test]
    fn downsampling_halves_the_length() {
        let s: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        assert_eq!(resample_to_16k(&s, 32_000).len(), 50);
    }

    /// Pre-refactor `on_rms` logic, kept as an independent oracle for `SpeculativeVad`.
    fn oracle(state: &mut (u64, u64, u64), rms: f32, threshold: f32) -> VadEvent {
        let (silent_for_ms, silence_ms, poll_ms) = (&mut state.0, state.1, state.2);
        if rms < threshold {
            let was_speaking = *silent_for_ms == 0;
            *silent_for_ms += poll_ms;
            if *silent_for_ms >= silence_ms {
                VadEvent::Confirmed
            } else if was_speaking {
                VadEvent::SpawnSpeculative
            } else {
                VadEvent::None
            }
        } else {
            let was_silent = *silent_for_ms != 0;
            *silent_for_ms = 0;
            if was_silent {
                VadEvent::DiscardSpeculative
            } else {
                VadEvent::None
            }
        }
    }

    #[test]
    fn the_split_changed_no_behaviour() {
        const THRESHOLD: f32 = 0.01;
        // Straddle the threshold and hit it exactly, where an inverted comparison hides.
        let levels = [0.5, 0.001, 0.01, 0.0099, 0.0101, 0.0, 0.2, 0.005, 0.5, 0.0];

        for &(silence_ms, poll_ms) in &[(300u64, 100u64), (90, 30), (1_000, 100), (30, 30)] {
            let mut vad = SpeculativeVad::new(silence_ms, poll_ms);
            let mut oracle_state = (0u64, silence_ms, poll_ms);
            let mut detector = RmsDetector::new(THRESHOLD);

            // Repeat so later pauses are covered, not just the first.
            for round in 0..4 {
                for (i, &level) in levels.iter().enumerate() {
                    // A constant frame whose RMS is exactly `level`.
                    let frame = [level; 8];
                    let got = vad.on_speech(detector.is_speech(&frame));
                    let want = oracle(&mut oracle_state, level, THRESHOLD);
                    assert_eq!(
                        got, want,
                        "round {round}, step {i}, level {level}, \
                         silence_ms {silence_ms}, poll_ms {poll_ms}"
                    );
                }
            }
        }
    }

    // ── Windower ──────────────────────────────────────────────────────────

    /// Collect every window a sequence of reads produces.
    fn windows_of(size: usize, reads: &[usize]) -> Vec<Vec<f32>> {
        let mut w = Windower::new(size);
        let mut out = Vec::new();
        let mut next = 0.0f32;
        for &n in reads {
            // Each sample is its own index, so a dropped or repeated one shows.
            let read: Vec<f32> = (0..n)
                .map(|_| {
                    next += 1.0;
                    next
                })
                .collect();
            w.push(&read, |win| out.push(win.to_vec()));
        }
        out
    }

    #[test]
    fn windows_are_exactly_consecutive_under_jitter() {
        // A 30 ms poll at 16 kHz is ~480 samples, never exactly; the model needs 512.
        let reads = [480, 512, 470, 490, 300, 700, 480, 480, 1, 999];
        let got = windows_of(512, &reads);

        assert!(!got.is_empty());
        let flat: Vec<f32> = got.concat();
        // No sample seen twice or skipped: the concatenation must be 1, 2, 3, ...
        let expected: Vec<f32> = (1..=flat.len()).map(|i| i as f32).collect();
        assert_eq!(flat, expected, "windows are not contiguous");
        for w in &got {
            assert_eq!(w.len(), 512, "a window came out the wrong size");
        }
    }

    #[test]
    fn a_short_read_emits_nothing_and_is_not_lost() {
        let mut w = Windower::new(512);
        let mut seen = 0;
        w.push(&[1.0; 100], |_| seen += 1);
        assert_eq!(seen, 0, "not enough for a window yet");
        assert_eq!(w.pending(), 100, "and the samples are held, not dropped");
        w.push(&[2.0; 412], |win| {
            seen += 1;
            assert_eq!(win.len(), 512);
        });
        assert_eq!(seen, 1, "the two reads together make one window");
        assert_eq!(w.pending(), 0);
    }

    #[test]
    fn one_long_read_emits_every_window_it_contains() {
        let mut w = Windower::new(512);
        let mut count = 0;
        // A late poll's backlog must all be processed, or the detector falls behind.
        w.push(&[0.5; 512 * 3 + 7], |_| count += 1);
        assert_eq!(count, 3);
        assert_eq!(w.pending(), 7);
    }

    #[test]
    fn reset_drops_the_leftover() {
        let mut w = Windower::new(512);
        w.push(&[1.0; 300], |_| unreachable!());
        w.reset();
        assert_eq!(w.pending(), 0);
        let mut seen = 0;
        // If the 300 had survived, 300 + 300 would have emitted a window.
        w.push(&[2.0; 300], |_| seen += 1);
        assert_eq!(seen, 0, "the pre-reset samples must not count");
    }

    #[test]
    fn a_nan_frame_counts_as_silence() {
        let mut d = RmsDetector::new(0.01);
        assert!(!d.is_speech(&[f32::NAN; 4]), "NaN is silence, deliberately");
        assert!(!d.is_speech(&[]));
    }

    #[test]
    fn the_detector_matches_the_comparison_it_replaced() {
        let mut d = RmsDetector::new(0.01);
        assert!(d.is_speech(&[0.5; 4]));
        assert!(!d.is_speech(&[0.001; 4]));
        assert!(d.is_speech(&[0.01; 4]), "the boundary belongs to speech");
        assert!(!d.is_speech(&[0.0099; 4]));
    }

    #[test]
    fn a_detector_can_be_swapped_at_runtime() {
        // Pins object safety, so the detector can be chosen at runtime.
        let mut boxed: Box<dyn SpeechDetector> = Box::new(RmsDetector::new(0.01));
        assert!(boxed.is_speech(&[0.5; 4]));
        boxed.reset();
        assert!(!boxed.is_speech(&[0.0; 4]));
    }

    #[test]
    fn the_vad_reports_a_silence_run_once_then_confirms() {
        const SILENCE: f32 = 0.001;
        const SPEECH: f32 = 0.5;
        const T: f32 = 0.01;
        let mut vad = SpeculativeVad::new(300, 100);

        assert_eq!(vad.on_speech(SPEECH >= T), VadEvent::None);
        assert_eq!(vad.on_speech(SILENCE >= T), VadEvent::SpawnSpeculative);
        assert_eq!(
            vad.on_speech(SILENCE >= T),
            VadEvent::None,
            "only once per run"
        );
        assert_eq!(vad.on_speech(SILENCE >= T), VadEvent::Confirmed);
    }

    #[test]
    fn resumed_speech_discards_the_speculative_result() {
        const SILENCE: f32 = 0.001;
        const SPEECH: f32 = 0.5;
        const T: f32 = 0.01;
        let mut vad = SpeculativeVad::new(1_000, 100);
        assert_eq!(vad.on_speech(SILENCE >= T), VadEvent::SpawnSpeculative);
        assert_eq!(vad.on_speech(SPEECH >= T), VadEvent::DiscardSpeculative);
        assert_eq!(vad.on_speech(SPEECH >= T), VadEvent::None);
        assert_eq!(vad.on_speech(SILENCE >= T), VadEvent::SpawnSpeculative);
    }

    #[test]
    fn the_real_jfk_fixture_has_a_pre_data_chunk_and_still_decodes() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("jfk.wav fixture missing — skipping");
            return;
        };

        // The fixture really does have the shape this test is about.
        let data_at = bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("fixture must have a data chunk")
            + 8;
        assert_ne!(
            data_at, 44,
            "fixture no longer exercises the pre-data-chunk case"
        );

        let got = decode_wav(&bytes).expect("real-world WAV must decode");
        assert_eq!(got.sample_rate, 16_000);

        // Length must match the declared data chunk, not the distance from 44.
        assert_eq!(got.samples.len(), (bytes.len() - data_at) / 2);

        // The first samples must be audio, not chunk bytes; jfk.wav opens on near-silence.
        assert!(
            got.samples[..64].iter().all(|s| s.abs() < 0.05),
            "leading samples look like chunk bytes, not audio"
        );

        // Contrast with a decoder that assumes data at byte 44.
        let old_len = (bytes.len() - 44) / 2;
        assert_eq!(
            old_len - got.samples.len(),
            (data_at - 44) / 2,
            "the old path emitted this many junk samples"
        );
    }
}
