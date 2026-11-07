//! A recording as the phone sent it to the 16 kHz mono 16-bit WAV the audio encoder takes.
//! Runs before the turn is saved, on the blocking pool; nothing past the duration limit is decoded.

use std::io::Cursor;

use pond_core::models::domain::audio_input::{
    check_wav, AudioLimitError, AUDIO_OPUS_MESSAGE, AUDIO_UNREADABLE_MESSAGE, ENGINE_SAMPLE_RATE,
};
use rubato::{
    calculate_cutoff, Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Input frames per resampler call: small enough that the buffers stay a few KB.
const RESAMPLE_CHUNK: usize = 1024;

/// The prepared recording.
#[derive(Debug)]
pub struct Normalized {
    pub wav: Vec<u8>,
    pub duration_ms: u64,
    /// What it was decoded from, for the log.
    pub source: String,
}

fn unreadable() -> AudioLimitError {
    AudioLimitError::Undecodable {
        message: AUDIO_UNREADABLE_MESSAGE,
    }
}

/// Decode `bytes` and return the engine's WAV, refusing past `max_ms` without decoding the rest.
/// A WAV already at 16 kHz mono 16-bit passes through untouched.
pub fn normalize(
    bytes: &[u8],
    mime_type: &str,
    max_ms: u64,
) -> Result<Normalized, AudioLimitError> {
    if let Ok(info) = check_wav(bytes) {
        if info.sample_rate == ENGINE_SAMPLE_RATE && info.channels == 1 {
            return Ok(Normalized {
                wav: bytes.to_vec(),
                duration_ms: info.duration_ms,
                source: "wav (as sent)".to_string(),
            });
        }
    }

    let mut hint = Hint::new();
    if let Some(ext) = extension_for(mime_type) {
        hint.with_extension(ext);
    }
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|_| unreadable())?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(unreadable)?;
    if track.codec_params.codec == CODEC_TYPE_OPUS {
        return Err(AudioLimitError::Undecodable {
            message: AUDIO_OPUS_MESSAGE,
        });
    }
    let track_id = track.id;
    let rate = track.codec_params.sample_rate.ok_or_else(unreadable)?;
    let max_frames = u64::from(rate) * max_ms / 1000;
    let too_long = |frames: u64| AudioLimitError::TooLong {
        ms: frames * 1000 / u64::from(rate),
        max_ms,
    };
    if let Some(declared) = track.codec_params.n_frames {
        if declared > max_frames {
            return Err(too_long(declared));
        }
    }
    let codec = symphonia::default::get_codecs()
        .get_codec(track.codec_params.codec)
        .map(|d| d.short_name.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|_| unreadable())?;

    let mut resampler = Mono16k::new(rate)?;
    let mut decoded_frames = 0u64;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(_) => return Err(unreadable()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // One corrupt packet costs a few milliseconds of sound, not the recording.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => return Err(unreadable()),
        };
        let spec = *decoded.spec();
        let frames = decoded.frames();
        if frames == 0 {
            continue;
        }
        decoded_frames += frames as u64;
        if decoded_frames > max_frames {
            return Err(too_long(decoded_frames));
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        resampler.push_interleaved(buf.samples(), spec.channels.count())?;
    }

    let pcm = resampler.finish(decoded_frames)?;
    if pcm.is_empty() {
        return Err(AudioLimitError::Empty);
    }
    let duration_ms = pcm.len() as u64 * 1000 / u64::from(ENGINE_SAMPLE_RATE);
    if duration_ms > max_ms {
        return Err(AudioLimitError::TooLong {
            ms: duration_ms,
            max_ms,
        });
    }
    Ok(Normalized {
        wav: wav_bytes(&pcm),
        duration_ms,
        source: format!("{codec} {rate} Hz"),
    })
}

fn extension_for(mime_type: &str) -> Option<&'static str> {
    let mime = mime_type.trim().to_ascii_lowercase();
    Some(match mime.split(';').next().unwrap_or("").trim() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "wav",
        _ => return None,
    })
}

/// Downmixes to mono and resamples to 16 kHz as packets arrive, so memory stays at the output.
struct Mono16k {
    rate: u32,
    resampler: Option<SincFixedIn<f32>>,
    pending: Vec<f32>,
    out: Vec<f32>,
}

impl Mono16k {
    fn new(rate: u32) -> Result<Self, AudioLimitError> {
        let resampler = if rate == ENGINE_SAMPLE_RATE {
            None
        } else {
            // A short sinc keeps the nano's CPU cost low; speech has nothing near the cutoff.
            let sinc_len = 64;
            let window = WindowFunction::BlackmanHarris2;
            let params = SincInterpolationParameters {
                sinc_len,
                f_cutoff: calculate_cutoff(sinc_len, window),
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window,
            };
            Some(
                SincFixedIn::<f32>::new(
                    f64::from(ENGINE_SAMPLE_RATE) / f64::from(rate),
                    1.0,
                    params,
                    RESAMPLE_CHUNK,
                    1,
                )
                .map_err(|_| unreadable())?,
            )
        };
        Ok(Self {
            rate,
            resampler,
            pending: Vec::with_capacity(RESAMPLE_CHUNK * 2),
            out: Vec::new(),
        })
    }

    fn push_interleaved(
        &mut self,
        samples: &[f32],
        channels: usize,
    ) -> Result<(), AudioLimitError> {
        let channels = channels.max(1);
        for frame in samples.chunks_exact(channels) {
            self.pending
                .push(frame.iter().sum::<f32>() / channels as f32);
        }
        let Some(resampler) = self.resampler.as_mut() else {
            self.out.append(&mut self.pending);
            return Ok(());
        };
        while self.pending.len() >= resampler.input_frames_next() {
            let take = resampler.input_frames_next();
            let chunk = [&self.pending[..take]];
            let waves = resampler
                .process(&chunk[..], None)
                .map_err(|_| unreadable())?;
            self.out.extend_from_slice(&waves[0]);
            self.pending.drain(..take);
        }
        Ok(())
    }

    /// The remaining input, with the resampler's delay dropped and the length set by the input.
    fn finish(mut self, input_frames: u64) -> Result<Vec<f32>, AudioLimitError> {
        let Some(mut resampler) = self.resampler.take() else {
            return Ok(self.out);
        };
        let delay = resampler.output_delay();
        let rate = u64::from(self.rate);
        let wanted = ((input_frames * u64::from(ENGINE_SAMPLE_RATE) + rate / 2) / rate) as usize;
        // Each flush feeds silence through the filter; a few are enough to clear its delay.
        for _ in 0..16 {
            if self.out.len() >= wanted + delay {
                break;
            }
            let tail = std::mem::take(&mut self.pending);
            // rubato reads an empty slice as a missing channel; `None` is its flush.
            let input = [tail.as_slice()];
            let waves = resampler
                .process_partial((!tail.is_empty()).then_some(&input[..]), None)
                .map_err(|_| unreadable())?;
            if waves[0].is_empty() {
                break;
            }
            self.out.extend_from_slice(&waves[0]);
        }
        let end = (wanted + delay).min(self.out.len());
        Ok(self.out[delay.min(end)..end].to_vec())
    }
}

/// 16-bit PCM mono at 16 kHz in a canonical 44-byte RIFF header.
fn wav_bytes(pcm: &[f32]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&ENGINE_SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(ENGINE_SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        let v = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::audio_input::{AUDIO_OPUS_MESSAGE, MAX_AUDIO_MS};

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!("../tests/fixtures/attachments/", $name)).as_slice()
        };
    }

    /// Zero crossings a second in the middle second: 880 for the fixtures' 440 Hz tone.
    fn crossings_per_second(wav: &[u8]) -> usize {
        let pcm: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let middle = &pcm[8_000..24_000];
        middle
            .windows(2)
            .filter(|w| (w[0] < 0) != (w[1] < 0))
            .count()
    }

    fn assert_two_seconds_of_440(name: &str, bytes: &[u8], mime: &str) {
        let out = normalize(bytes, mime, MAX_AUDIO_MS).unwrap_or_else(|e| panic!("{name}: {e}"));
        let info = check_wav(&out.wav).unwrap();
        assert_eq!((info.sample_rate, info.channels), (16_000, 1), "{name}");
        assert!(
            (1_950..=2_100).contains(&out.duration_ms),
            "{name} decoded to {} ms",
            out.duration_ms
        );
        let crossings = crossings_per_second(&out.wav);
        assert!(
            (870..=890).contains(&crossings),
            "{name}: {crossings} crossings a second, the tone moved"
        );
    }

    #[test]
    fn phone_formats_decode_to_two_seconds_of_the_same_tone() {
        assert_two_seconds_of_440("m4a", fixture!("tone.m4a"), "audio/mp4");
        assert_two_seconds_of_440("aac", fixture!("tone.aac"), "audio/aac");
        assert_two_seconds_of_440("mp3", fixture!("tone.mp3"), "audio/mpeg");
        assert_two_seconds_of_440("flac", fixture!("tone.flac"), "audio/flac");
        assert_two_seconds_of_440("ogg", fixture!("tone.ogg"), "audio/ogg");
        assert_two_seconds_of_440("wav", fixture!("tone-22k-stereo.wav"), "audio/wav");
    }

    #[test]
    fn a_wrong_mime_label_does_not_matter_the_bytes_decide() {
        assert_two_seconds_of_440("m4a as mpeg", fixture!("tone.m4a"), "audio/mpeg");
    }

    #[test]
    fn opus_and_garbage_are_refused_with_a_sentence() {
        assert_eq!(
            normalize(fixture!("tone.opus.ogg"), "audio/ogg", MAX_AUDIO_MS).unwrap_err(),
            AudioLimitError::Undecodable {
                message: AUDIO_OPUS_MESSAGE
            }
        );
        assert_eq!(
            normalize(
                b"ID3\x04 not really an mp3 at all",
                "audio/mpeg",
                MAX_AUDIO_MS
            )
            .unwrap_err(),
            unreadable()
        );
    }

    #[test]
    fn a_recording_past_the_limit_is_refused_not_cut() {
        let err = normalize(fixture!("tone.m4a"), "audio/mp4", 1_000).unwrap_err();
        assert!(
            matches!(err, AudioLimitError::TooLong { max_ms: 1_000, .. }),
            "{err:?}"
        );
        assert_eq!(err.code(), "audio_too_long");
    }

    #[test]
    fn a_wav_already_in_the_engines_shape_passes_through_untouched() {
        let first = normalize(fixture!("tone.mp3"), "audio/mpeg", MAX_AUDIO_MS).unwrap();
        let again = normalize(&first.wav, "audio/wav", MAX_AUDIO_MS).unwrap();
        assert_eq!(again.wav, first.wav);
        assert_eq!(again.source, "wav (as sent)");
    }
}
