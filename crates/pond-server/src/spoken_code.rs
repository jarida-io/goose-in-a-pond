//! The pond's [`CodeAnnouncer`]: the configured voice when there is one, else short English clips
//! compiled into the binary, since a fresh pond may have no voice downloaded yet.

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::models::services::voice::quiet;
use pond_core::network::ports::code_announcer::CodeAnnouncer;
use pond_voice::dsp::{decode_wav, encode_wav_pcm16, f32_to_pcm16};

/// Kokoro-82M, voice `af_heart`; `assets/spoken-code/README.md` says how to remake them.
const DIGIT_CLIPS: [&[u8]; 10] = [
    include_bytes!("../assets/spoken-code/0.wav"),
    include_bytes!("../assets/spoken-code/1.wav"),
    include_bytes!("../assets/spoken-code/2.wav"),
    include_bytes!("../assets/spoken-code/3.wav"),
    include_bytes!("../assets/spoken-code/4.wav"),
    include_bytes!("../assets/spoken-code/5.wav"),
    include_bytes!("../assets/spoken-code/6.wav"),
    include_bytes!("../assets/spoken-code/7.wav"),
    include_bytes!("../assets/spoken-code/8.wav"),
    include_bytes!("../assets/spoken-code/9.wav"),
];
const INTRO_CLIP: &[u8] = include_bytes!("../assets/spoken-code/intro.wav");
const AGAIN_CLIP: &[u8] = include_bytes!("../assets/spoken-code/again.wav");

/// Between digits, so each is heard on its own.
const DIGIT_GAP: Duration = Duration::from_millis(300);
/// Around the two readings, so "once more" reads as a new start.
const PHRASE_GAP: Duration = Duration::from_millis(700);
/// How long the clips wait for other audio (music) to pause before playing anyway.
const WAIT_FOR_QUIET: Duration = Duration::from_millis(1500);

/// What the configured voice says. Digits are comma-separated so each is read on its own.
pub fn spoken_sentence(digits: &str) -> String {
    let spaced = digits
        .chars()
        .map(String::from)
        .collect::<Vec<_>>()
        .join(", ");
    format!("Your Goose pairing code is {spaced}. Once more: {spaced}.")
}

/// The bundled clips joined into one mono WAV: intro, the digits, "once more", the digits.
pub fn clip_sequence(digits: &str) -> Result<Vec<u8>> {
    let decode = |clip: &[u8]| decode_wav(clip).context("a bundled spoken-code clip is corrupt");
    let intro = decode(INTRO_CLIP)?;
    let rate = intro.sample_rate;
    let silence = |gap: Duration| vec![0f32; (rate as u128 * gap.as_millis() / 1000) as usize];

    let mut reading = Vec::new();
    for (i, c) in digits.chars().enumerate() {
        let Some(d) = c.to_digit(10) else {
            bail!("a pairing code is digits only");
        };
        let clip = decode(DIGIT_CLIPS[d as usize])?;
        if clip.sample_rate != rate {
            bail!("the bundled spoken-code clips disagree on their sample rate");
        }
        if i > 0 {
            reading.extend(silence(DIGIT_GAP));
        }
        reading.extend(clip.samples);
    }
    let again = decode(AGAIN_CLIP)?;
    if again.sample_rate != rate {
        bail!("the bundled spoken-code clips disagree on their sample rate");
    }

    let mut samples = intro.samples;
    samples.extend(silence(PHRASE_GAP));
    samples.extend_from_slice(&reading);
    samples.extend(silence(PHRASE_GAP));
    samples.extend(again.samples);
    samples.extend(silence(PHRASE_GAP));
    samples.extend(reading);
    Ok(encode_wav_pcm16(&f32_to_pcm16(&samples), rate))
}

/// Opens the default output device for each announcement and closes it after: a code is read
/// a few times an hour at most, and a held device would block the voice loop's own.
fn play_on_default_device(wav: Vec<u8>) -> Result<()> {
    let keeper = pond_audio_out::AudioKeeper::try_new("spoken-code-audio")?;
    let interrupted = AtomicBool::new(false);
    let utterance = AtomicU64::new(1);
    pond_audio_out::play_wav(wav, &keeper.handle, &interrupted, &utterance, None)
}

fn output_device_present() -> bool {
    use cpal::traits::HostTrait;
    cpal::default_host().default_output_device().is_some()
}

pub struct SpeakerCodeAnnouncer {
    tts: Option<Arc<dyn VoiceOutput>>,
}

impl SpeakerCodeAnnouncer {
    pub fn new(tts: Option<Arc<dyn VoiceOutput>>) -> Self {
        Self { tts }
    }

    async fn play_clips(digits: &str) -> Result<()> {
        let wav = clip_sequence(digits)?;
        // Music pauses for the clips as it does for the voice (Spotify's Developer Policy III.7).
        let quiet = quiet::installed();
        let _hold = quiet.as_ref().map(|q| q.hold());
        if let Some(quiet) = &quiet {
            quiet.until_quiet(WAIT_FOR_QUIET).await;
        }
        tokio::task::spawn_blocking(move || play_on_default_device(wav))
            .await
            .context("the spoken-code playback task panicked")?
    }
}

#[async_trait]
impl CodeAnnouncer for SpeakerCodeAnnouncer {
    async fn announce_code(&self, digits: &str) -> Result<()> {
        if let Some(tts) = &self.tts {
            tts.begin_utterance();
            let spoken = tts.speak(&spoken_sentence(digits)).await;
            tts.end_utterance();
            match spoken {
                Ok(()) => return Ok(()),
                Err(error) => tracing::warn!(
                    %error,
                    "the voice could not read the pairing code; playing the bundled clips"
                ),
            }
        }
        Self::play_clips(digits).await
    }

    async fn is_available(&self) -> bool {
        tokio::task::spawn_blocking(output_device_present)
            .await
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(&str, &[u8]); 12] = [
        ("0", DIGIT_CLIPS[0]),
        ("1", DIGIT_CLIPS[1]),
        ("2", DIGIT_CLIPS[2]),
        ("3", DIGIT_CLIPS[3]),
        ("4", DIGIT_CLIPS[4]),
        ("5", DIGIT_CLIPS[5]),
        ("6", DIGIT_CLIPS[6]),
        ("7", DIGIT_CLIPS[7]),
        ("8", DIGIT_CLIPS[8]),
        ("9", DIGIT_CLIPS[9]),
        ("intro", INTRO_CLIP),
        ("again", AGAIN_CLIP),
    ];

    /// The canonical 44-byte header's channel count, rate and bit depth.
    fn header(wav: &[u8]) -> (u16, u32, u16) {
        (
            u16::from_le_bytes([wav[22], wav[23]]),
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            u16::from_le_bytes([wav[34], wav[35]]),
        )
    }

    #[test]
    fn every_clip_decodes_as_short_mono_16_bit_pcm_at_most_24_khz() {
        let mut total = 0;
        for (name, clip) in ALL {
            let (channels, rate, bits) = header(clip);
            assert_eq!(channels, 1, "{name} is not mono");
            assert_eq!(bits, 16, "{name} is not 16-bit");
            assert!(rate <= 24_000, "{name} is {rate} Hz");
            let decoded = decode_wav(clip).unwrap_or_else(|e| panic!("{name}: {e}"));
            let secs = decoded.samples.len() as f32 / decoded.sample_rate as f32;
            assert!((0.2..=3.0).contains(&secs), "{name} lasts {secs:.2} s");
            assert!(
                decoded.samples.iter().any(|s| s.abs() > 0.1),
                "{name} is silent"
            );
            total += clip.len();
        }
        assert!(total <= 500 * 1024, "the clips come to {total} bytes");
    }

    #[test]
    fn the_sequence_reads_the_code_twice_and_never_needs_a_voice() {
        let wav = clip_sequence("482913").unwrap();
        let decoded = decode_wav(&wav).unwrap();
        let secs = decoded.samples.len() as f32 / decoded.sample_rate as f32;
        assert!((5.0..30.0).contains(&secs), "{secs:.1} s");
        assert!(clip_sequence("48a913").is_err());
    }

    #[test]
    fn the_voice_reads_each_digit_on_its_own_twice() {
        assert_eq!(
            spoken_sentence("482913"),
            "Your Goose pairing code is 4, 8, 2, 9, 1, 3. Once more: 4, 8, 2, 9, 1, 3."
        );
    }

    struct FailingVoice;

    #[async_trait]
    impl VoiceOutput for FailingVoice {
        async fn speak(&self, _text: &str) -> Result<()> {
            anyhow::bail!("no voice")
        }
    }

    struct RecordingVoice(std::sync::Mutex<Vec<String>>);

    #[async_trait]
    impl VoiceOutput for RecordingVoice {
        async fn speak(&self, text: &str) -> Result<()> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_working_voice_reads_the_sentence_and_the_clips_stay_unused() {
        let voice = Arc::new(RecordingVoice(Default::default()));
        let announcer = SpeakerCodeAnnouncer::new(Some(voice.clone()));
        announcer.announce_code("123456").await.unwrap();
        assert_eq!(
            voice.0.lock().unwrap().as_slice(),
            [spoken_sentence("123456")]
        );
    }

    /// Needs a speaker: `cargo test -p pond-server --bin pond-server spoken_code -- --ignored`.
    #[tokio::test]
    #[ignore = "plays audio on the host's default output device"]
    async fn a_failing_voice_falls_back_to_the_clips_out_loud() {
        let announcer = SpeakerCodeAnnouncer::new(Some(Arc::new(FailingVoice)));
        announcer.announce_code("482913").await.unwrap();
    }
}
