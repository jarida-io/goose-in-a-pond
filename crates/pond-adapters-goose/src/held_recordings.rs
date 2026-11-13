//! Recordings the model heard, kept in memory so a later turn hands it the sound again and not
//! a note about it. Goose's stored conversation carries only a reference, so no recording is
//! written to disk, and after a restart an earlier one reads as a note.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use goose::conversation::message::{Message, MessageContent};
use pond_core::models::domain::audio_input::HISTORY_VOICE_NOTE_PLACEHOLDER;

/// The type a stored message's audio part has when its data is a reference to a held recording.
pub(crate) const HELD_RECORDING_MIME: &str = "audio/x-giap-held";

/// Base64 bytes held at once, the oldest going first: about nine minutes of speech.
const MAX_HELD_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone)]
struct Held {
    id: String,
    data: Arc<str>,
    mime_type: Arc<str>,
    /// What the engine's cache knows the recording by.
    file: u64,
}

static HELD: Mutex<VecDeque<Held>> = Mutex::new(VecDeque::new());

fn held() -> std::sync::MutexGuard<'static, VecDeque<Held>> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps a recording and returns the reference a stored message carries for it.
pub(crate) fn hold(data: &str, mime_type: &str) -> String {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap_or_else(|_| data.as_bytes().to_vec());
    let file = goose::providers::local_inference::media_content_hash(&bytes, true);
    let id = format!("{file:016x}");
    let mut held = held();
    if held.iter().any(|clip| clip.id == id) {
        return id;
    }
    held.push_back(Held {
        id: id.clone(),
        data: data.into(),
        mime_type: mime_type.into(),
        file,
    });
    let mut bytes: usize = held.iter().map(|clip| clip.data.len()).sum();
    while bytes > MAX_HELD_BYTES && held.len() > 1 {
        bytes -= held.pop_front().map_or(0, |clip| clip.data.len());
    }
    id
}

fn find(id: &str) -> Option<Held> {
    held().iter().find(|clip| clip.id == id).cloned()
}

fn forget(ids: &[String]) {
    held().retain(|clip| !ids.contains(&clip.id));
}

fn reference(part: &MessageContent) -> Option<&str> {
    match part {
        MessageContent::Audio(audio) if audio.mime_type == HELD_RECORDING_MIME => {
            Some(audio.data.as_str())
        }
        _ => None,
    }
}

/// `messages` with every reference replaced by a note, for a call that is not the chat itself
/// (a summary, a compaction): it has no use for the sound, and would load the encoder for it.
/// Nothing is forgotten. `None` when no message carries a reference.
pub(crate) fn as_notes(messages: &[Message]) -> Option<Vec<Message>> {
    messages
        .iter()
        .any(|m| m.content.iter().any(|part| reference(part).is_some()))
        .then(|| {
            messages
                .iter()
                .map(|m| {
                    let mut noted = m.clone();
                    for part in &mut noted.content {
                        if reference(part).is_some() {
                            *part = MessageContent::text(HISTORY_VOICE_NOTE_PLACEHOLDER);
                        }
                    }
                    noted
                })
                .collect()
        })
}

/// `messages` with each reference replaced by its recording, for the model to hear. This turn's
/// was admitted when it arrived; the earlier ones go as sound when `hearable` says the model can
/// take them in now, given what the engine's cache knows them by. One that cannot, or is no
/// longer held, becomes a note and stays one, so the prompt does not flip between the two.
/// `None` when no message carries a reference.
pub(crate) fn resolve(
    messages: &[Message],
    hearable: impl FnOnce(&[u64]) -> bool,
) -> Option<Vec<Message>> {
    if !messages
        .iter()
        .any(|m| m.content.iter().any(|part| reference(part).is_some()))
    {
        return None;
    }
    let current = messages
        .iter()
        .rposition(|m| m.role == rmcp::model::Role::User && !crate::goose_agent::has_tool_parts(m));
    let earlier: Vec<Held> = messages
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != current)
        .flat_map(|(_, m)| m.content.iter().filter_map(reference))
        .filter_map(find)
        .collect();
    let hear_earlier =
        earlier.is_empty() || hearable(&earlier.iter().map(|clip| clip.file).collect::<Vec<_>>());
    if !hear_earlier {
        forget(
            &earlier
                .iter()
                .map(|clip| clip.id.clone())
                .collect::<Vec<_>>(),
        );
        tracing::info!(
            target: "giap::trace",
            kind = "voice_note_unattached",
            recordings = earlier.len(),
            reason = "no_room_to_hear",
            "earlier recordings go as notes: the model cannot take them in now"
        );
    }
    Some(
        messages
            .iter()
            .enumerate()
            .map(|(index, m)| {
                if !m.content.iter().any(|part| reference(part).is_some()) {
                    return m.clone();
                }
                let mut resolved = m.clone();
                resolved.content = m
                    .content
                    .iter()
                    .map(|part| match reference(part) {
                        None => part.clone(),
                        Some(id) => match find(id) {
                            Some(clip) if hear_earlier || Some(index) == current => {
                                MessageContent::audio(&*clip.data, &*clip.mime_type)
                            }
                            _ => MessageContent::text(HISTORY_VOICE_NOTE_PLACEHOLDER),
                        },
                    })
                    .collect();
                resolved
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(seed: u8) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode([seed; 64])
    }

    fn spoken(text: &str, id: &str) -> Message {
        Message::user()
            .with_text(text)
            .with_audio(id, HELD_RECORDING_MIME)
    }

    fn audio_of(m: &Message) -> Vec<(String, String)> {
        m.content
            .iter()
            .filter_map(|part| match part {
                MessageContent::Audio(a) => Some((a.data.clone(), a.mime_type.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_earlier_recording_goes_to_the_model_as_sound() {
        let (first, second) = (clip(1), clip(2));
        let messages = vec![
            spoken("one", &hold(&first, "audio/wav")),
            Message::assistant().with_text("answer"),
            spoken("two", &hold(&second, "audio/wav")),
        ];
        let resolved = resolve(&messages, |files| {
            assert_eq!(files.len(), 1, "only the earlier recording is asked about");
            true
        })
        .expect("references were found");
        assert_eq!(audio_of(&resolved[0]), [(first, "audio/wav".to_string())]);
        assert_eq!(audio_of(&resolved[2]), [(second, "audio/wav".to_string())]);
        assert_eq!(resolved[1], messages[1]);
    }

    #[test]
    fn one_the_model_cannot_take_in_becomes_a_note_and_stays_one() {
        let (first, second) = (clip(3), clip(4));
        let messages = vec![
            spoken("one", &hold(&first, "audio/wav")),
            Message::assistant().with_text("answer"),
            spoken("two", &hold(&second, "audio/wav")),
        ];
        let resolved = resolve(&messages, |_| false).expect("references were found");
        assert!(audio_of(&resolved[0]).is_empty());
        assert_eq!(
            resolved[0].as_concat_text(),
            format!("one\n{HISTORY_VOICE_NOTE_PLACEHOLDER}")
        );
        assert_eq!(
            audio_of(&resolved[2]).len(),
            1,
            "this turn's is still heard"
        );

        let again = resolve(&messages, |_| panic!("nothing earlier is held any more"))
            .expect("references were found");
        assert!(audio_of(&again[0]).is_empty());
    }

    #[test]
    fn a_reference_nothing_is_held_for_reads_as_a_note() {
        let messages = vec![
            spoken("before a restart", "00000000deadbeef"),
            Message::assistant().with_text("answer"),
            Message::user().with_text("and now?"),
        ];
        let resolved = resolve(&messages, |_| true).expect("a reference was found");
        assert!(audio_of(&resolved[0]).is_empty());
        assert!(resolved[0]
            .as_concat_text()
            .contains(HISTORY_VOICE_NOTE_PLACEHOLDER));
        assert!(resolve(&[Message::user().with_text("plain")], |_| true).is_none());
    }

    #[test]
    fn a_side_call_reads_notes_and_the_chat_keeps_its_recordings() {
        let first = clip(5);
        let messages = vec![
            spoken("one", &hold(&first, "audio/wav")),
            Message::assistant().with_text("answer"),
        ];
        let noted = as_notes(&messages).expect("a reference was found");
        assert!(audio_of(&noted[0]).is_empty());
        assert!(noted[0]
            .as_concat_text()
            .contains(HISTORY_VOICE_NOTE_PLACEHOLDER));
        assert!(as_notes(&[Message::user().with_text("plain")]).is_none());

        let resolved = resolve(&messages, |_| true).expect("a reference was found");
        assert_eq!(audio_of(&resolved[0]), [(first, "audio/wav".to_string())]);
    }
}
