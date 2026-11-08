//! Speech outside a turn, like a reminder: it queues for the audio bus like every other sound.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::models::services::voice::quiet;
use pond_voice::text::split_text;

use crate::{bus, Claim, Floor};

/// The longest an announcement queues for the speaker before it is dropped.
const WAITS_AT_MOST: Duration = Duration::from_secs(300);
/// How long it waits for other audio (music) to pause before it speaks anyway.
const WAIT_FOR_QUIET: Duration = Duration::from_millis(1500);

type Player = Arc<dyn Fn(&Floor, Vec<u8>) -> Result<()> + Send + Sync>;

/// Says text on the voice once it holds the audio bus: behind any turn in the queue, never over one.
#[derive(Clone)]
pub struct Announcer {
    voice: Arc<dyn VoiceOutput>,
}

impl Announcer {
    pub fn new(voice: Arc<dyn VoiceOutput>) -> Self {
        Self { voice }
    }

    /// Returns once the text has been said, or failed to be.
    pub async fn speak_text(&self, text: &str) -> Result<()> {
        self.say(text, Arc::new(|floor: &Floor, wav| floor.play_wav(wav)))
            .await
    }

    /// Holds the bus for the whole text and synthesises each unit while the one before it plays.
    async fn say(&self, text: &str, play: Player) -> Result<()> {
        let units = split_text(text);
        if units.is_empty() {
            return Ok(());
        }
        let floor = tokio::task::spawn_blocking(|| {
            bus().claim(Claim::Notice, Some(WAITS_AT_MOST), || true)
        })
        .await
        .context("the announcement's claim panicked")?
        .ok_or_else(|| anyhow!("the speaker stayed busy"))?;
        // Music pauses for it as for every sound the pond makes (Spotify's Developer Policy III.7).
        let quiet = quiet::installed();
        let _quiet = quiet.as_ref().map(|q| q.hold());
        if let Some(quiet) = &quiet {
            quiet.until_quiet(WAIT_FOR_QUIET).await;
        }
        let floor = Arc::new(floor);
        let mut playing: Option<tokio::task::JoinHandle<Result<()>>> = None;
        for (i, unit) in units.into_iter().enumerate() {
            let Some(wav) = self.voice.synthesize_unit(&unit).await? else {
                if i > 0 {
                    bail!("the voice stopped synthesising mid-announcement");
                }
                // An output that cannot synthesise does not play on the bus; it says it its own way.
                drop((floor, _quiet));
                return self.voice.speak(text).await;
            };
            if let Some(previous) = playing.take() {
                previous
                    .await
                    .context("an announcement's playback panicked")??;
            }
            if !floor.keep() {
                return Ok(());
            }
            crate::tap::report_spoken(&unit.text);
            let (floor, play) = (floor.clone(), play.clone());
            playing = Some(tokio::task::spawn_blocking(move || play(&floor, wav)));
        }
        if let Some(last) = playing {
            last.await
                .context("an announcement's playback panicked")??;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Synth(AtomicUsize);

    #[async_trait]
    impl VoiceOutput for Synth {
        async fn speak(&self, _text: &str) -> Result<()> {
            unreachable!("an announcement synthesises and plays on the bus itself")
        }

        async fn synthesize(&self, _text: &str) -> Result<Option<Vec<u8>>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Some(pond_voice::dsp::encode_wav_pcm16(&[], 24_000)))
        }
    }

    fn counting(played: Arc<AtomicUsize>) -> Player {
        Arc::new(move |floor: &Floor, _wav| {
            assert!(floor.keep(), "played only while holding the bus");
            played.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    #[tokio::test]
    async fn every_unit_is_synthesised_and_played_while_holding_the_bus() {
        let voice = Arc::new(Synth::default());
        let played = Arc::new(AtomicUsize::new(0));
        let text =
            "First clause of speech, followed by the second clause here. And a third sentence.";
        Announcer::new(voice.clone())
            .say(text, counting(played.clone()))
            .await
            .unwrap();
        assert!(voice.0.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            voice.0.load(Ordering::SeqCst),
            played.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn an_announcement_queues_behind_whatever_holds_the_bus() {
        let turn = tokio::task::spawn_blocking(|| bus().claim(Claim::Turn, None, || true))
            .await
            .unwrap()
            .unwrap();
        let played = Arc::new(AtomicUsize::new(0));
        let announcing = tokio::spawn({
            let played = played.clone();
            async move {
                Announcer::new(Arc::new(Synth::default()))
                    .say("Your timer is done.", counting(played))
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(played.load(Ordering::SeqCst), 0, "not over the turn");
        drop(turn);
        announcing.await.unwrap().unwrap();
        assert_eq!(played.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn what_an_announcement_says_reaches_the_spoken_tap() {
        // The only test that sets the spoken tap, so it needs no lock.
        let said = Arc::new(Mutex::new(Vec::<String>::new()));
        crate::set_spoken_tap(Some({
            let said = said.clone();
            Arc::new(move |text: &str| said.lock().unwrap().push(text.to_string()))
        }));
        Announcer::new(Arc::new(Synth::default()))
            .say(
                "Your timer is done. The tea is ready.",
                counting(Arc::new(AtomicUsize::new(0))),
            )
            .await
            .unwrap();
        crate::set_spoken_tap(None);
        let said = said.lock().unwrap().join(" ");
        assert!(
            said.contains("Your timer is done") && said.contains("The tea is ready"),
            "{said}"
        );
    }
}
