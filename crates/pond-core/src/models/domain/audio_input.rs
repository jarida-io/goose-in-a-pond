//! Voice notes a chat turn carries to the model as sound, not as a transcript.
//!
//! The model hears them through its picture encoder's audio tower, so a voice note works exactly
//! where that encoder is ready and its header declares a tower the engine can run.

use super::gguf::GgufLayout;
use super::image_limits::decoded_len;
use super::message::AudioAttachment;
use super::vision_encoder::EncoderState;

/// One recording per turn: a held-mic note, or one file from the phone.
pub const MAX_AUDIO_PER_TURN: usize = 1;

/// Three minutes: 4,500 audio tokens on Gemma 4 (25 a second). The nano prefills a multimodal
/// turn at about 560 tokens a second, so that adds about 8 s to a turn in its 64k window.
pub const MAX_AUDIO_MS: u64 = 180_000;

/// The file as sent: three minutes of M4A, MP3 or Ogg at any common bitrate, or of 16 kHz mono
/// WAV (5.8 MB). CD-quality WAV or FLAC fits about 45 s.
pub const MAX_AUDIO_BYTES: usize = 8 * 1024 * 1024;

/// What the route accepts; the pond decodes every one to 16 kHz mono WAV before the engine sees it.
pub const SUPPORTED_AUDIO_MIME_TYPES: &[&str] = &[
    "audio/wav",
    "audio/x-wav",
    "audio/wave",
    "audio/vnd.wave",
    "audio/mpeg",
    "audio/mp3",
    "audio/mp4",
    "audio/x-m4a",
    "audio/m4a",
    "audio/aac",
    "audio/flac",
    "audio/x-flac",
    "audio/ogg",
];

/// The rates a WAV may have to pass straight through; the encoder itself takes 16 kHz.
const SAMPLE_RATES: std::ops::RangeInclusive<u32> = 8_000..=48_000;

/// What the engine is handed: the encoder's own rate, one channel, 16-bit PCM.
pub const ENGINE_SAMPLE_RATE: u32 = 16_000;

/// Audio projectors the vendored mtmd runs and the pond has heard answer end to end.
pub const AUDIO_PROJECTORS: &[&str] = &["gemma4a"];

/// What the user row stores in place of an unnamed recording, which the pond does not keep.
pub const VOICE_NOTE_PLACEHOLDER: &str = "[Voice note]";

/// What later turns see where an earlier turn's recording was: the engine heard it then. Nothing
/// here may read as a missing recording, or E4B tells the household it cannot hear the new one.
pub const HISTORY_VOICE_NOTE_PLACEHOLDER: &str =
    "[Voice note from an earlier turn, heard and answered then]";

/// The row's mark for one recording: the words a voice turn's client heard in it, else its file
/// name when the phone sent one.
pub fn persisted_mark(clip: &AudioAttachment) -> String {
    if let Some(words) = filled(clip.heard_as.as_deref()) {
        return words.to_string();
    }
    match filled(clip.name.as_deref()) {
        Some(name) => format!("[Audio: {}]", super::document_input::display_name(name)),
        None => VOICE_NOTE_PLACEHOLDER.to_string(),
    }
}

fn filled(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Whether an encoder with this header carries an audio tower the engine runs.
pub fn encoder_hears(layout: &GgufLayout) -> bool {
    layout.has_audio_encoder == Some(true)
        && layout
            .audio_projector_type
            .as_deref()
            .is_some_and(|p| AUDIO_PROJECTORS.contains(&p))
}

/// What the household reads when a file does not decode, naming what does.
pub const AUDIO_UNREADABLE_MESSAGE: &str = "The pond could not read this recording. Send it as \
     M4A, MP3, AAC, FLAC, Ogg Vorbis or WAV.";

/// Opus decodes nowhere on the pond yet, so it gets its own sentence.
pub const AUDIO_OPUS_MESSAGE: &str = "This recording is Opus, which the pond cannot read yet. \
     Send it as M4A, MP3, AAC, FLAC, Ogg Vorbis or WAV.";

/// Why a turn's recording was rejected before it reached the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioLimitError {
    TooManyClips {
        count: usize,
        max: usize,
    },
    UnsupportedMimeType {
        mime_type: String,
    },
    Empty,
    TooLarge {
        bytes: usize,
        max: usize,
    },
    NotBase64,
    NotAWav,
    UnsupportedEncoding {
        detail: String,
    },
    /// The bytes would not decode; the household sentence says what would.
    Undecodable {
        message: &'static str,
    },
    TooLong {
        ms: u64,
        max_ms: u64,
    },
}

impl std::fmt::Display for AudioLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        match self {
            Self::TooManyClips { count, max } => write!(
                f,
                "Too many recordings in one message: {count} attached, at most {max} per message."
            ),
            Self::UnsupportedMimeType { mime_type } => write!(
                f,
                "The pond cannot read \"{mime_type}\" recordings. Send it as M4A, MP3, AAC, \
                 FLAC, Ogg Vorbis or WAV."
            ),
            Self::Empty => write!(f, "The recording carries no sound."),
            Self::TooLarge { bytes, max } => write!(
                f,
                "The recording is {:.1} MB, over the {:.0} MB limit.",
                mb(*bytes),
                mb(*max)
            ),
            Self::NotBase64 => write!(f, "The recording's data is not valid base64."),
            Self::NotAWav => write!(f, "The recording is not a WAV file."),
            Self::UnsupportedEncoding { detail } => write!(
                f,
                "The WAV must be 16-bit PCM, mono or stereo, 8-48 kHz ({detail})."
            ),
            Self::Undecodable { message } => f.write_str(message),
            Self::TooLong { ms, max_ms } => write!(
                f,
                "The recording is {:.0} s long, over the {} s limit. Trim it and send it again.",
                *ms as f64 / 1000.0,
                max_ms / 1000
            ),
        }
    }
}

impl std::error::Error for AudioLimitError {}

impl AudioLimitError {
    /// `true` for 413, `false` for 400 or 415 (see [`Self::is_unsupported_media`]).
    pub fn is_too_large(&self) -> bool {
        matches!(
            self,
            Self::TooManyClips { .. } | Self::TooLarge { .. } | Self::TooLong { .. }
        )
    }

    /// `true` for 415: the bytes are not a recording the pond can read.
    pub fn is_unsupported_media(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedMimeType { .. }
                | Self::NotAWav
                | Self::UnsupportedEncoding { .. }
                | Self::Undecodable { .. }
        )
    }

    /// The API's `code` field.
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "audio_too_large",
            Self::TooLong { .. } => "audio_too_long",
            _ => "audio_invalid",
        }
    }
}

/// Count, type and size, from the base64 alone: runs before anything is decoded.
pub fn validate_turn_audio(audio: &[AudioAttachment]) -> Result<(), AudioLimitError> {
    if audio.len() > MAX_AUDIO_PER_TURN {
        return Err(AudioLimitError::TooManyClips {
            count: audio.len(),
            max: MAX_AUDIO_PER_TURN,
        });
    }
    for clip in audio {
        let mime = clip.mime_type.trim().to_ascii_lowercase();
        let base = mime.split(';').next().unwrap_or("").trim();
        if !SUPPORTED_AUDIO_MIME_TYPES.contains(&base) {
            return Err(AudioLimitError::UnsupportedMimeType {
                mime_type: clip.mime_type.clone(),
            });
        }
        let bytes = decoded_len(&clip.data);
        if bytes == 0 {
            return Err(AudioLimitError::Empty);
        }
        if bytes > MAX_AUDIO_BYTES {
            return Err(AudioLimitError::TooLarge {
                bytes,
                max: MAX_AUDIO_BYTES,
            });
        }
    }
    Ok(())
}

/// A WAV's format and length, as its `fmt ` and `data` chunks state them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub duration_ms: u64,
}

/// Read and check a decoded voice note: 16-bit PCM, one or two channels, 8-48 kHz, at most a
/// minute. A `data` chunk longer than the file (a streaming writer's placeholder) counts as
/// what is present.
pub fn check_wav(bytes: &[u8]) -> Result<WavInfo, AudioLimitError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(AudioLimitError::NotAWav);
    }
    let mut format: Option<(u16, u16, u32, u32, u16)> = None;
    let mut at = 12usize;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let body = at + 8;
        if id == b"fmt " {
            if size < 16 || body + 16 > bytes.len() {
                return Err(AudioLimitError::NotAWav);
            }
            let u16_at = |o: usize| u16::from_le_bytes([bytes[body + o], bytes[body + o + 1]]);
            let u32_at = |o: usize| {
                u32::from_le_bytes([
                    bytes[body + o],
                    bytes[body + o + 1],
                    bytes[body + o + 2],
                    bytes[body + o + 3],
                ])
            };
            format = Some((u16_at(0), u16_at(2), u32_at(4), u32_at(8), u16_at(14)));
        } else if id == b"data" {
            let (tag, channels, sample_rate, byte_rate, bits) =
                format.ok_or(AudioLimitError::NotAWav)?;
            check_encoding(tag, channels, sample_rate, byte_rate, bits)?;
            let len = size.min(bytes.len() - body) as u64;
            let duration_ms = len * 1000 / u64::from(byte_rate);
            if duration_ms == 0 {
                return Err(AudioLimitError::Empty);
            }
            if duration_ms > MAX_AUDIO_MS {
                return Err(AudioLimitError::TooLong {
                    ms: duration_ms,
                    max_ms: MAX_AUDIO_MS,
                });
            }
            return Ok(WavInfo {
                sample_rate,
                channels,
                bits_per_sample: bits,
                duration_ms,
            });
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    Err(AudioLimitError::NotAWav)
}

fn check_encoding(
    tag: u16,
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    bits: u16,
) -> Result<(), AudioLimitError> {
    let unsupported = |detail: String| Err(AudioLimitError::UnsupportedEncoding { detail });
    if tag != 1 {
        return unsupported(format!("format tag {tag}, not PCM"));
    }
    if bits != 16 {
        return unsupported(format!("{bits}-bit samples"));
    }
    if !(1..=2).contains(&channels) {
        return unsupported(format!("{channels} channels"));
    }
    if !SAMPLE_RATES.contains(&sample_rate) {
        return unsupported(format!("{sample_rate} Hz"));
    }
    if byte_rate != sample_rate * u32::from(channels) * 2 {
        return unsupported(format!("byte rate {byte_rate} does not match the format"));
    }
    Ok(())
}

/// Which 409 a voice-note turn gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRefusalCode {
    /// A different model (or provider) is the fix.
    Unsupported,
    /// Waiting for the model's add-on is the fix.
    NotReady,
}

impl AudioRefusalCode {
    /// The API's `code` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unsupported => "audio_unsupported",
            Self::NotReady => "audio_not_ready",
        }
    }
}

/// A voice-note turn refused before anything is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRefusal {
    pub code: AudioRefusalCode,
    pub message: String,
}

pub const AUDIO_UNSUPPORTED_MESSAGE: &str = "This model cannot listen to voice notes. Send the \
     note as text, or choose a model on this device that hears them, such as Gemma 4 E2B or E4B.";

pub const AUDIO_NOT_READY_MESSAGE: &str = "This model will hear voice notes once its picture and \
     sound add-on is ready on the Models page. Send the note as text meanwhile.";

/// Whether a voice-note turn may go ahead, from the agent's audio state. Unlike pictures, an
/// unknown state refuses: no other path carries a recording, so it would be dropped silently.
pub fn audio_refusal_for(state: Option<&EncoderState>) -> Option<AudioRefusal> {
    match state {
        Some(s) if s.is_ready() => None,
        Some(s) if s.is_pending() => Some(AudioRefusal {
            code: AudioRefusalCode::NotReady,
            message: AUDIO_NOT_READY_MESSAGE.to_string(),
        }),
        _ => Some(AudioRefusal {
            code: AudioRefusalCode::Unsupported,
            message: AUDIO_UNSUPPORTED_MESSAGE.to_string(),
        }),
    }
}

/// `voice_notes` on `GET /models/capabilities`, from the same verdict the 409s give, so a client
/// can tell a pond that takes `audio` from an older one (which never sends the field).
pub fn voice_notes_status(state: Option<&EncoderState>) -> &'static str {
    match audio_refusal_for(state) {
        None => "ready",
        Some(AudioRefusal {
            code: AudioRefusalCode::NotReady,
            ..
        }) => "not_ready",
        Some(_) => "unsupported",
    }
}

/// Whether the encoder that hears a recording can take this turn now, as memory stands. A turn
/// it cannot take goes as the words Whisper made of the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HearingRoom {
    Loaded,
    Fits {
        available_mb: u64,
        needed_mb: u64,
    },
    Short {
        available_mb: u64,
        needed_mb: u64,
    },
    /// Nothing to judge by, so the recording goes to the model as before.
    Unknown,
}

impl HearingRoom {
    pub fn admits(self) -> bool {
        !matches!(self, HearingRoom::Short { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(data: &str, mime: &str) -> AudioAttachment {
        AudioAttachment {
            data: data.to_string(),
            mime_type: mime.to_string(),
            name: None,
            heard_as: None,
        }
    }

    /// A PCM WAV header plus `data_len` bytes of silence.
    fn wav(sample_rate: u32, channels: u16, bits: u16, data_len: usize) -> Vec<u8> {
        let block = channels * bits / 8;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * u32::from(block)).to_le_bytes());
        out.extend_from_slice(&block.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        out.resize(out.len() + data_len, 0);
        out
    }

    #[test]
    fn a_phone_note_passes_and_reports_its_length() {
        let info = check_wav(&wav(16_000, 1, 16, 32_000 * 12)).unwrap();
        assert_eq!(info.sample_rate, 16_000);
        assert_eq!(info.channels, 1);
        assert_eq!(info.duration_ms, 12_000);
    }

    #[test]
    fn the_longest_note_at_16k_mono_fits_both_limits() {
        let bytes = wav(16_000, 1, 16, 32_000 * (MAX_AUDIO_MS as usize / 1000));
        assert!(bytes.len() <= MAX_AUDIO_BYTES);
        assert_eq!(check_wav(&bytes).unwrap().duration_ms, MAX_AUDIO_MS);
    }

    #[test]
    fn past_the_limit_is_too_long_even_when_small() {
        let err = check_wav(&wav(8_000, 1, 16, 16_000 * 181)).unwrap_err();
        assert_eq!(
            err,
            AudioLimitError::TooLong {
                ms: 181_000,
                max_ms: MAX_AUDIO_MS
            }
        );
        assert!(err.is_too_large());
        assert_eq!(err.code(), "audio_too_long");
    }

    #[test]
    fn chunks_before_data_are_skipped() {
        let mut bytes = wav(16_000, 1, 16, 32_000);
        let list = [b"LIST".as_slice(), &3u32.to_le_bytes(), b"abc\0"].concat();
        bytes.splice(36..36, list);
        assert_eq!(check_wav(&bytes).unwrap().duration_ms, 1_000);
    }

    #[test]
    fn a_data_size_past_the_end_counts_what_is_there() {
        let mut bytes = wav(16_000, 1, 16, 32_000);
        bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(check_wav(&bytes).unwrap().duration_ms, 1_000);
    }

    #[test]
    fn non_wav_and_non_pcm_are_unsupported_media() {
        assert_eq!(
            check_wav(b"ID3\x04 not a wav"),
            Err(AudioLimitError::NotAWav)
        );
        let float = {
            let mut b = wav(16_000, 1, 16, 3_200);
            b[20..22].copy_from_slice(&3u16.to_le_bytes());
            b
        };
        let err = check_wav(&float).unwrap_err();
        assert!(err.is_unsupported_media(), "{err}");
        assert!(check_wav(&wav(16_000, 1, 8, 1_600))
            .unwrap_err()
            .is_unsupported_media());
        assert!(check_wav(&wav(96_000, 1, 16, 19_200))
            .unwrap_err()
            .is_unsupported_media());
    }

    #[test]
    fn a_header_with_no_samples_is_empty() {
        assert_eq!(
            check_wav(&wav(16_000, 1, 16, 0)),
            Err(AudioLimitError::Empty)
        );
    }

    #[test]
    fn the_request_limits_count_type_and_size() {
        assert_eq!(validate_turn_audio(&[]), Ok(()));
        assert_eq!(
            validate_turn_audio(&[clip("UklGRg==", "audio/wav")]),
            Ok(())
        );
        assert_eq!(
            validate_turn_audio(&[clip("UklGRg==", "Audio/WAV; codecs=1")]),
            Ok(())
        );
        assert!(matches!(
            validate_turn_audio(&[clip("UklGRg==", "audio/wav"), clip("UklGRg==", "audio/wav")]),
            Err(AudioLimitError::TooManyClips { count: 2, max: 1 })
        ));
        for phone in [
            "audio/mpeg",
            "audio/mp4",
            "audio/x-m4a",
            "audio/aac",
            "audio/flac",
            "audio/ogg",
        ] {
            assert_eq!(
                validate_turn_audio(&[clip("UklGRg==", phone)]),
                Ok(()),
                "{phone}"
            );
        }
        assert!(validate_turn_audio(&[clip("UklGRg==", "audio/webm")])
            .unwrap_err()
            .is_unsupported_media());
        assert_eq!(
            validate_turn_audio(&[clip("", "audio/wav")]),
            Err(AudioLimitError::Empty)
        );
        let big = "A".repeat((MAX_AUDIO_BYTES + 3) * 4 / 3);
        let err = validate_turn_audio(&[clip(&big, "audio/wav")]).unwrap_err();
        assert!(err.is_too_large());
        assert_eq!(err.code(), "audio_too_large");
    }

    #[test]
    fn only_a_ready_audio_encoder_lets_a_note_through() {
        assert_eq!(
            audio_refusal_for(Some(&EncoderState::Ready { bytes: None })),
            None
        );
        let pending = audio_refusal_for(Some(&EncoderState::Verifying)).unwrap();
        assert_eq!(pending.code.as_str(), "audio_not_ready");
        for state in [
            None,
            Some(EncoderState::Unknown),
            Some(EncoderState::NotDeclared),
            Some(EncoderState::NotOnThisDevice),
        ] {
            let refusal = audio_refusal_for(state.as_ref()).unwrap();
            assert_eq!(refusal.code.as_str(), "audio_unsupported", "{state:?}");
        }
    }

    #[test]
    fn voice_notes_status_names_the_refusal_a_turn_would_get() {
        assert_eq!(
            voice_notes_status(Some(&EncoderState::Ready { bytes: None })),
            "ready"
        );
        assert_eq!(
            voice_notes_status(Some(&EncoderState::Downloading { done: 1, total: 2 })),
            "not_ready"
        );
        assert_eq!(
            voice_notes_status(Some(&EncoderState::NotDeclared)),
            "unsupported"
        );
        assert_eq!(voice_notes_status(None), "unsupported");
    }

    #[test]
    fn a_named_file_is_marked_by_name_and_a_note_as_a_voice_note() {
        let mut named = clip("UklGRg==", "audio/mp4");
        named.name = Some("../lecture 3.m4a".to_string());
        assert_eq!(persisted_mark(&named), "[Audio: lecture 3.m4a]");
        let spoken = AudioAttachment {
            heard_as: Some(" what time is it ".to_string()),
            ..clip("UklGRg==", "audio/wav")
        };
        assert_eq!(persisted_mark(&spoken), "what time is it");
        assert_eq!(
            persisted_mark(&clip("UklGRg==", "audio/wav")),
            VOICE_NOTE_PLACEHOLDER
        );
    }

    #[test]
    fn hearing_needs_the_flag_and_a_projector_the_engine_runs() {
        let layout = |flag: Option<bool>, projector: Option<&str>| GgufLayout {
            has_audio_encoder: flag,
            audio_projector_type: projector.map(str::to_string),
            ..GgufLayout::default()
        };
        assert!(encoder_hears(&layout(Some(true), Some("gemma4a"))));
        assert!(!encoder_hears(&layout(Some(false), Some("gemma4a"))));
        assert!(!encoder_hears(&layout(None, None)));
        assert!(!encoder_hears(&layout(Some(true), Some("whisper"))));
    }
}
