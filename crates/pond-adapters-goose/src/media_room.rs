//! What an engine call does when memory is too short for the encoder its recording or picture
//! needs. It never fails the turn: it frees what reloads by itself, then drops earlier media,
//! then waits for memory to come back, and last takes the turn's own media as words. The live
//! turn is told at each step why its reply has not started.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use goose::conversation::message::{Message, MessageContent};
use goose::providers::base::MessageStream;
use goose::providers::local_inference::load_error::ModelLoadError;
use goose_providers::errors::ProviderError;
use pond_core::models::domain::audio_input::HISTORY_VOICE_NOTE_PLACEHOLDER;
use pond_core::models::ports::media_room::MediaRoom;

static ROOM: RwLock<Option<Arc<dyn MediaRoom>>> = RwLock::new(None);

/// What the engine path asks of the pond when memory is short. Unset, the steps that need it
/// are skipped.
pub fn set_media_room(room: Arc<dyn MediaRoom>) {
    *ROOM.write().unwrap_or_else(|e| e.into_inner()) = Some(room);
}

fn media_room() -> Option<Arc<dyn MediaRoom>> {
    ROOM.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// How long a call waits for memory to come back before it takes its media as words: about what
/// a reply being spoken, or the Memory model's pass, holds.
const ROOM_WAIT: Duration = Duration::from_secs(20);
const ROOM_RECHECK: Duration = Duration::from_millis(500);

/// The steps, in the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The call as the shim built it.
    AsAsked,
    /// After giving back the models that reload by themselves.
    RoomMade,
    /// With only this turn's own recording or picture left to take in.
    EarlierDropped,
    /// After waiting for memory to come back.
    Waited,
    /// With this turn's own media as words: nothing left needs the encoder.
    AsWords,
}

/// What a call's first item said about room for its encoder.
pub(crate) enum Peeked {
    Go(Result<MessageStream, ProviderError>),
    NoRoom {
        needed_mb: u64,
        error: ProviderError,
    },
}

fn no_room(error: &ProviderError) -> Option<u64> {
    match ModelLoadError::from_provider_error(error)? {
        ModelLoadError::NoRoomForEncoder { needed_mb, .. } => Some(needed_mb),
        _ => None,
    }
}

/// Reads a call's first item: the engine refuses an encoder before it writes anything, so a
/// refusal is always first. Any other first item is handed back in front of the rest.
pub(crate) async fn peek(result: Result<MessageStream, ProviderError>) -> Peeked {
    use futures::StreamExt;
    let mut stream = match result {
        Err(error) => {
            return match no_room(&error) {
                Some(needed_mb) => Peeked::NoRoom { needed_mb, error },
                None => Peeked::Go(Err(error)),
            }
        }
        Ok(stream) => stream,
    };
    match stream.next().await {
        None => Peeked::Go(Ok(Box::pin(futures::stream::empty()))),
        Some(Err(error)) if no_room(&error).is_some() => Peeked::NoRoom {
            needed_mb: no_room(&error).unwrap_or_default(),
            error,
        },
        Some(first) => Peeked::Go(Ok(Box::pin(
            futures::stream::once(async move { first }).chain(stream),
        ))),
    }
}

/// The message whose media this turn brought: the last user message that is not a tool's.
fn current(messages: &[Message]) -> Option<usize> {
    messages
        .iter()
        .rposition(|m| m.role == rmcp::model::Role::User && !crate::goose_agent::has_tool_parts(m))
}

fn is_media(part: &MessageContent) -> bool {
    matches!(part, MessageContent::Image(_) | MessageContent::Audio(_))
}

/// Whether any message hands the engine a recording or a picture.
pub(crate) fn carries_media(messages: &[Message]) -> bool {
    messages.iter().any(|m| m.content.iter().any(is_media))
}

/// What the household is told while the call waits.
pub(crate) fn making_room_line(messages: &[Message]) -> &'static str {
    let parts = current(messages).map_or(&[][..], |i| &messages[i].content[..]);
    let hears = parts.iter().any(|p| matches!(p, MessageContent::Audio(_)));
    let sees = parts.iter().any(|p| matches!(p, MessageContent::Image(_)));
    match (hears, sees) {
        (true, false) => "Your pond is busy. Making room to hear you…",
        (false, true) => "Your pond is busy. Making room to look at your picture…",
        _ => "Your pond is busy. Making room for your message…",
    }
}

pub(crate) const STILL_MAKING_ROOM: &str =
    "Still making room. Your pond will answer as soon as it can.";

/// Gives back what reloads by itself; the names of what was given back.
pub(crate) async fn make_room() -> Vec<&'static str> {
    match media_room() {
        Some(room) => room.make_room().await,
        None => Vec::new(),
    }
}

/// `messages` with every recording and picture but this turn's own as notes, so the encoder
/// needs only the towers this turn uses. Tool messages are never rewritten (pairing is checked).
/// `None` when nothing earlier carries media.
pub(crate) fn drop_earlier_media(messages: &[Message]) -> Option<Vec<Message>> {
    let current = current(messages);
    let earlier = |i: usize, m: &Message| {
        Some(i) != current
            && !crate::goose_agent::has_tool_parts(m)
            && m.content.iter().any(is_media)
    };
    if !messages.iter().enumerate().any(|(i, m)| earlier(i, m)) {
        return None;
    }
    Some(
        messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                if !earlier(i, m) {
                    return m.clone();
                }
                let mut noted = crate::goose_agent::cap_message_images(m, 0, &m.as_concat_text());
                for part in &mut noted.content {
                    if matches!(part, MessageContent::Audio(_)) {
                        *part = MessageContent::text(HISTORY_VOICE_NOTE_PLACEHOLDER);
                    }
                }
                noted
            })
            .collect(),
    )
}

/// MiB the kernel says could be had now, where it says.
fn available_mb() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// Waits until `needed_mb` is available, for at most [`ROOM_WAIT`]. True when it is.
pub(crate) async fn wait_for_room(needed_mb: u64) -> bool {
    let started = Instant::now();
    let got = loop {
        match available_mb() {
            None => break false,
            Some(mb) if mb >= needed_mb => break true,
            Some(_) if started.elapsed() >= ROOM_WAIT => break false,
            Some(_) => tokio::time::sleep(ROOM_RECHECK).await,
        }
    };
    tracing::info!(
        target: "giap::trace",
        kind = "media_room_waited",
        needed_mb,
        available_mb = available_mb().unwrap_or_default(),
        waited_ms = started.elapsed().as_millis() as u64,
        got_room = got,
        "waited for memory to take in a turn's recording or picture"
    );
    got
}

const PICTURE_NOT_SEEN: &str = "[A picture came with this message, but the pond had no memory \
     free to look at it. Say so in one sentence and ask them to send it again in a minute.]";
const VOICE_NOTE_NOT_HEARD: &str = "[A voice note came with this message, but the pond had no \
     memory free to hear it and no words were made out. Ask them to say it again in a moment.]";

/// `messages` with this turn's recordings as their words (the client's, else a transcription)
/// and its pictures as a note the model answers from. `None` when it carries neither.
pub(crate) async fn as_words(messages: &[Message]) -> Option<Vec<Message>> {
    use base64::Engine as _;
    let index = current(messages)?;
    if !messages[index].content.iter().any(is_media) {
        return None;
    }
    let room = media_room();
    let mut out = messages.to_vec();
    let mut parts = Vec::with_capacity(out[index].content.len());
    for part in std::mem::take(&mut out[index].content) {
        parts.push(match part {
            MessageContent::Image(_) => MessageContent::text(PICTURE_NOT_SEEN),
            MessageContent::Audio(audio) => {
                let words = match crate::held_recordings::words_for(&audio.data) {
                    Some(words) => Some(words),
                    None => match (
                        room.as_ref(),
                        base64::engine::general_purpose::STANDARD.decode(&audio.data),
                    ) {
                        (Some(room), Ok(wav)) => room.words_of(wav).await,
                        _ => None,
                    },
                };
                match words.filter(|w| !w.trim().is_empty()) {
                    Some(words) => {
                        MessageContent::text(format!("The voice note, as words: {words}"))
                    }
                    None => MessageContent::text(VOICE_NOTE_NOT_HEARD),
                }
            }
            other => other,
        });
    }
    out[index].content = parts;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(m: &Message) -> String {
        m.as_concat_text()
    }

    #[test]
    fn a_no_room_refusal_is_told_apart_from_other_errors() {
        let refusal: ProviderError = ModelLoadError::NoRoomForEncoder {
            needed_mb: 1_201,
            available_mb: 1_021,
        }
        .into();
        assert_eq!(no_room(&refusal), Some(1_201));
        assert_eq!(
            no_room(&ProviderError::ExecutionError("decode failed".into())),
            None
        );
    }

    #[tokio::test]
    async fn a_refusal_as_the_first_item_is_caught_and_anything_else_streams_whole() {
        let refusal: ProviderError = ModelLoadError::NoRoomForEncoder {
            needed_mb: 851,
            available_mb: 600,
        }
        .into();
        let refused: MessageStream = Box::pin(futures::stream::iter(vec![Err(refusal)]));
        assert!(matches!(
            peek(Ok(refused)).await,
            Peeked::NoRoom { needed_mb: 851, .. }
        ));

        let reply: MessageStream = Box::pin(futures::stream::iter(vec![
            Ok((Some(Message::assistant().with_text("one")), None)),
            Ok((Some(Message::assistant().with_text("two")), None)),
        ]));
        let Peeked::Go(Ok(stream)) = peek(Ok(reply)).await else {
            panic!("a reply was taken for a refusal");
        };
        use futures::StreamExt;
        let said: Vec<String> = stream
            .map(|item| texts(&item.unwrap().0.unwrap()))
            .collect()
            .await;
        assert_eq!(said, vec!["one", "two"], "the first item was lost or moved");
    }

    #[test]
    fn earlier_media_become_notes_and_this_turns_stays() {
        let messages = vec![
            Message::user()
                .with_text("look")
                .with_image("aGVsbG8=", "image/png"),
            Message::assistant().with_text("a cat"),
            Message::user()
                .with_text("listen")
                .with_audio("UklGRg==", "audio/wav"),
        ];
        let dropped = drop_earlier_media(&messages).expect("an earlier picture to drop");
        assert!(!dropped[0].content.iter().any(is_media), "{:?}", dropped[0]);
        assert!(dropped[2]
            .content
            .iter()
            .any(|p| matches!(p, MessageContent::Audio(_))));
        assert_eq!(
            drop_earlier_media(&dropped),
            None,
            "nothing earlier is left to drop"
        );
    }

    #[tokio::test]
    async fn this_turns_media_go_as_words_and_notes() {
        let messages = vec![Message::user()
            .with_text("what is this")
            .with_image("aGVsbG8=", "image/png")
            .with_audio("bm8gd29yZHM=", "audio/wav")];
        let worded = as_words(&messages).await.expect("media to take as words");
        assert!(!carries_media(&worded));
        let text = texts(&worded[0]);
        assert!(text.contains("no memory free to look at it"), "{text}");
        assert!(text.contains("no memory free to hear it"), "{text}");
    }

    #[test]
    fn the_line_says_what_the_room_is_for() {
        let heard = vec![Message::user().with_audio("UklGRg==", "audio/wav")];
        assert_eq!(
            making_room_line(&heard),
            "Your pond is busy. Making room to hear you…"
        );
        let seen = vec![Message::user().with_image("aGVsbG8=", "image/png")];
        assert!(making_room_line(&seen).contains("look at your picture"));
    }
}
