//! Speech outside a turn, like a reminder: it queues for the audio bus like every other sound,
//! yields to the person, and picks up again where it was cut off.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_voice::text::{split_text, SpeechUnit};

use crate::{bus, Claim, Floor};

/// The longest an announcement queues for the speaker before it is dropped.
const WAITS_AT_MOST: Duration = Duration::from_secs(300);

type Player = Arc<dyn Fn(&Floor, Vec<u8>) -> Result<()> + Send + Sync>;

enum Heard {
    All,
    /// The bus was wanted back during this unit, counted from the first one played.
    CutIn(usize),
    /// The voice plays (or prints) its words itself, so they never reach the bus.
    Unsynthesisable,
}

/// Says text on the voice once it holds the audio bus: behind any turn in the queue, never over
/// one, and from the cut sentence again after a turn or a ping took the bus from it.
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

    async fn say(&self, text: &str, play: Player) -> Result<()> {
        let units = split_text(text);
        let mut from = 0;
        while from < units.len() {
            let floor = tokio::task::spawn_blocking(|| {
                bus().claim(Claim::Notice, Some(WAITS_AT_MOST), || true)
            })
            .await
            .context("the announcement's claim panicked")?
            .ok_or_else(|| anyhow!("the speaker stayed busy"))?;
            match self.play_units(&units[from..], floor, &play).await? {
                Heard::All => break,
                Heard::CutIn(unit) => {
                    from += unit;
                    tracing::info!(
                        kind = "announcement_resumes",
                        from,
                        "an announcement gave the speaker up and will pick up again"
                    );
                }
                Heard::Unsynthesisable => return self.voice.speak(text).await,
            }
        }
        Ok(())
    }

    /// Plays each unit on `floor` while the next is synthesised.
    async fn play_units(&self, units: &[SpeechUnit], floor: Floor, play: &Player) -> Result<Heard> {
        let floor = Arc::new(floor);
        let mut playing: Option<tokio::task::JoinHandle<Result<()>>> = None;
        for (k, unit) in units.iter().enumerate() {
            let Some(wav) = self.voice.synthesize_unit(unit).await? else {
                if k == 0 {
                    return Ok(Heard::Unsynthesisable);
                }
                bail!("the voice stopped synthesising mid-announcement");
            };
            if let Some(previous) = playing.take() {
                previous
                    .await
                    .context("an announcement's playback panicked")??;
                if !floor.keep() {
                    return Ok(Heard::CutIn(k - 1));
                }
            }
            crate::tap::report_spoken(&unit.text);
            let (floor, play) = (floor.clone(), play.clone());
            playing = Some(tokio::task::spawn_blocking(move || play(&floor, wav)));
        }
        if let Some(last) = playing {
            last.await
                .context("an announcement's playback panicked")??;
            if !floor.keep() {
                return Ok(Heard::CutIn(units.len() - 1));
            }
        }
        Ok(Heard::All)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Synthesises each unit as a WAV whose sample rate is its length, so a player can tell them
    /// apart.
    #[derive(Default)]
    struct Synth(AtomicUsize);

    #[async_trait]
    impl VoiceOutput for Synth {
        async fn speak(&self, _text: &str) -> Result<()> {
            unreachable!("an announcement synthesises and plays on the bus itself")
        }

        async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Some(pond_voice::dsp::encode_wav_pcm16(
                &[],
                8_000 + text.len() as u32,
            )))
        }
    }

    fn rate(wav: &[u8]) -> u32 {
        u32::from_le_bytes(wav[24..28].try_into().unwrap())
    }

    fn recording(played: Arc<Mutex<Vec<u32>>>) -> Player {
        Arc::new(move |floor: &Floor, wav| {
            assert!(floor.keep(), "played only while holding the bus");
            played.lock().unwrap().push(rate(&wav));
            Ok(())
        })
    }

    #[tokio::test]
    async fn every_unit_is_synthesised_and_played_while_holding_the_bus() {
        let voice = Arc::new(Synth::default());
        let played = Arc::new(Mutex::new(Vec::new()));
        let text =
            "First clause of speech, followed by the second clause here. And a third sentence.";
        Announcer::new(voice.clone())
            .say(text, recording(played.clone()))
            .await
            .unwrap();
        assert!(voice.0.load(Ordering::SeqCst) >= 2);
        assert_eq!(voice.0.load(Ordering::SeqCst), played.lock().unwrap().len());
    }

    #[tokio::test]
    async fn an_announcement_queues_behind_a_turn() {
        let turn = tokio::task::spawn_blocking(|| bus().claim(Claim::Turn, None, || true))
            .await
            .unwrap()
            .unwrap();
        let played = Arc::new(Mutex::new(Vec::new()));
        let announcing = tokio::spawn({
            let played = played.clone();
            async move {
                Announcer::new(Arc::new(Synth::default()))
                    .say("Your timer is done.", recording(played))
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(played.lock().unwrap().is_empty(), "not over the turn");
        drop(turn);
        announcing.await.unwrap().unwrap();
        assert_eq!(played.lock().unwrap().len(), 1);
    }

    /// A turn arrives during the second sentence: the announcement yields, waits for the turn,
    /// and says the second sentence again before the third.
    #[tokio::test]
    async fn a_cut_announcement_picks_up_at_the_sentence_it_was_cut_in() {
        let played = Arc::new(Mutex::new(Vec::new()));
        let plays = Arc::new(AtomicUsize::new(0));
        let player: Player = Arc::new({
            let played = played.clone();
            move |floor: &Floor, wav| {
                played.lock().unwrap().push(rate(&wav));
                if plays.fetch_add(1, Ordering::SeqCst) == 1 {
                    std::thread::spawn(|| {
                        let turn = bus().claim(Claim::Turn, None, || true);
                        std::thread::sleep(Duration::from_millis(30));
                        drop(turn);
                    });
                    while floor.keep() {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
                Ok(())
            }
        });
        Announcer::new(Arc::new(Synth::default()))
            .say("The tea is ready. Pour it now. Then sit down.", player)
            .await
            .unwrap();
        let played = played.lock().unwrap().clone();
        assert_eq!(played.len(), 4, "{played:?}");
        assert_eq!(played[1], played[2], "the cut sentence is said again");
        assert_ne!(played[2], played[3]);
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
                recording(Arc::new(Mutex::new(Vec::new()))),
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
