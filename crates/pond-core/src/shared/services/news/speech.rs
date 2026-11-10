//! A digest as it is heard: the whole of it for the phone's Listen, and its opening and headings
//! on the house speaker. Markdown and citations are for the eye only.

use anyhow::Result;

use crate::models::ports::voice_output::VoiceOutput;
use crate::shared::domain::news::Digest;
use crate::shared::services::chat::{
    quiet_hours_cover, LocalTimeOfDay, SpeechRefusal, UnpromptedSpeech,
};
use crate::user_data::domain::settings::Settings;

/// Whether the house speaker may read a digest unasked now, as a schedule's speech is decided:
/// outside the household's quiet hours, with the pond allowed to speak first, and never while a
/// conversation holds the model.
pub fn house_may_speak(
    settings: Option<&Settings>,
    now: LocalTimeOfDay,
    turn_in_flight: bool,
) -> UnpromptedSpeech {
    use UnpromptedSpeech::Refused;
    let Some(settings) = settings else {
        return Refused(SpeechRefusal::SettingsUnreadable);
    };
    match quiet_hours_cover(&settings.quiet_hours_start, &settings.quiet_hours_end, now) {
        None => return Refused(SpeechRefusal::QuietHoursUnreadable),
        Some(true) => return Refused(SpeechRefusal::QuietHours),
        Some(false) => {}
    }
    if !settings.unprompted_speech_enabled {
        return Refused(SpeechRefusal::NotEnabled);
    }
    if turn_in_flight {
        return Refused(SpeechRefusal::MidConversation);
    }
    UnpromptedSpeech::Spoken
}

/// Every part of the digest the phone's Listen plays, in order: the intro, then each section's
/// heading and body.
pub fn listen_parts(digest: &Digest) -> Vec<String> {
    let mut parts = vec![plain(&digest.intro)];
    for section in &digest.sections {
        parts.push(sentence(&plain(&section.heading)));
        for paragraph in section.body.split("\n\n") {
            let paragraph = plain(paragraph);
            if !paragraph.is_empty() {
                parts.push(paragraph);
            }
        }
    }
    parts.retain(|p| !p.is_empty());
    parts
}

/// The whole digest in the pond's voice, as one WAV: synthesised a unit at a time, as a turn's
/// reply is, since one call for the lot would run past what the voice takes at once. `None` when
/// the voice cannot synthesise.
pub async fn listen_wav(voice: &dyn VoiceOutput, digest: &Digest) -> Result<Option<Vec<u8>>> {
    let script = listen_parts(digest).join("\n\n");
    let mut parts = Vec::new();
    for unit in pond_voice::text::split_text(&script) {
        match voice.synthesize_unit(&unit).await? {
            Some(wav) => parts.push(wav),
            None if parts.is_empty() => return Ok(None),
            None => anyhow::bail!("the voice stopped synthesising mid-digest"),
        }
    }
    Ok(super::wav::join(&parts, 0))
}

/// What the house speaker says at the digest time: the member's name, the intro and the
/// headings.
pub fn speaker_text(digest: &Digest, name: Option<&str>) -> String {
    let mut text = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => format!("{name}, your news is ready. "),
        None => "Your news is ready. ".to_string(),
    };
    text.push_str(&sentence(&plain(&digest.intro)));
    let headings: Vec<String> = digest
        .sections
        .iter()
        .map(|s| sentence(&plain(&s.heading)))
        .filter(|h| !h.is_empty())
        .collect();
    if !headings.is_empty() {
        text.push_str(" In it: ");
        text.push_str(&headings.join(" "));
    }
    text.trim().to_string()
}

fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() || text.ends_with(['.', '!', '?']) {
        text.to_string()
    } else {
        format!("{text}.")
    }
}

/// Markdown as plain speech: links read as their words, citations and pictures dropped, and
/// emphasis, heading, list and quote marks gone.
pub fn plain(markdown: &str) -> String {
    let mut lines = Vec::new();
    for line in markdown.lines() {
        let mut line = line.trim();
        line = line.trim_start_matches('#').trim_start();
        line = line.trim_start_matches('>').trim_start();
        for marker in ["- ", "* ", "+ "] {
            if let Some(rest) = line.strip_prefix(marker) {
                line = rest;
            }
        }
        if let Some((number, rest)) = line.split_once(". ") {
            if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
                line = rest;
            }
        }
        lines.push(links_as_words(line));
    }
    let text = lines.join(" ");
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '_'))
        .collect();
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" .", ".")
        .replace(" ,", ",")
}

/// `[words](url)` as its words; a citation (`[2](url)`) and a picture (`![..](..)`) as nothing.
fn links_as_words(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let picture = open > 0 && rest[..open].ends_with('!');
        let before = if picture {
            &rest[..open - 1]
        } else {
            &rest[..open]
        };
        let after = &rest[open + 1..];
        let Some(close) = after.find("](") else {
            out.push_str(&rest[..=open]);
            rest = after;
            continue;
        };
        let words = &after[..close];
        let tail = &after[close + 2..];
        let Some(end) = tail.find(')') else {
            out.push_str(&rest[..=open]);
            rest = after;
            continue;
        };
        out.push_str(before);
        let citation = !words.is_empty() && words.chars().all(|c| c.is_ascii_digit());
        if !picture && !citation {
            out.push_str(words);
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::news::{DigestSection, InterestRef};
    use chrono::{NaiveDate, Utc};

    fn digest() -> Digest {
        Digest {
            id: "d1".into(),
            profile_id: "m".into(),
            day: NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
            title: "Sifuna's rally".into(),
            intro: "Two things today. Both are local.".into(),
            sections: vec![DigestSection {
                interest: InterestRef {
                    id: "i".into(),
                    topic: "Kenyan civic politics".into(),
                },
                heading: "Sifuna takes **Linda Mwananchi** to Kamukunji".into(),
                body: "The rally drew a crowd [1](https://a.example/1).\n\n- Next stop is \
                       [Kisumu](https://b.example/k) [1](https://a.example/1)[2](https://b.example/2)."
                    .into(),
                pictures: vec![],
                references: vec![],
            }],
            written_at: Utc::now(),
        }
    }

    #[test]
    fn listen_reads_the_intro_then_each_heading_and_body_as_plain_words() {
        assert_eq!(
            listen_parts(&digest()),
            vec![
                "Two things today. Both are local.",
                "Sifuna takes Linda Mwananchi to Kamukunji.",
                "The rally drew a crowd.",
                "Next stop is Kisumu.",
            ]
        );
    }

    struct Voice;

    #[async_trait::async_trait]
    impl VoiceOutput for Voice {
        async fn speak(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
            Ok(Some(super::super::wav::pcm16(1000, &vec![1; text.len()])))
        }
    }

    #[tokio::test]
    async fn listen_is_one_recording_of_every_part() {
        let wav = listen_wav(&Voice, &digest()).await.unwrap().unwrap();
        let script = listen_parts(&digest()).join("\n\n");
        let units = pond_voice::text::split_text(&script);
        assert!(units.len() >= 4, "a unit per sentence or so");
        let samples: usize = units.iter().map(|u| u.text.len()).sum();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(wav.len(), 44 + 2 * samples, "every unit, in one recording");
    }

    #[test]
    fn the_speaker_says_the_name_the_intro_and_the_headings() {
        assert_eq!(
            speaker_text(&digest(), Some("Jerry")),
            "Jerry, your news is ready. Two things today. Both are local. In it: Sifuna takes \
             Linda Mwananchi to Kamukunji."
        );
    }

    #[test]
    fn the_house_speaks_only_outside_quiet_hours_when_allowed_and_free() {
        let mut settings = Settings {
            quiet_hours_start: "22:00".into(),
            quiet_hours_end: "07:00".into(),
            unprompted_speech_enabled: true,
            ..Settings::default()
        };
        let at = |h, m| LocalTimeOfDay::new(h, m).unwrap();
        assert_eq!(
            house_may_speak(Some(&settings), at(7, 0), false),
            UnpromptedSpeech::Spoken
        );
        assert_eq!(
            house_may_speak(Some(&settings), at(6, 59), false),
            UnpromptedSpeech::Refused(SpeechRefusal::QuietHours)
        );
        assert_eq!(
            house_may_speak(Some(&settings), at(7, 0), true),
            UnpromptedSpeech::Refused(SpeechRefusal::MidConversation)
        );
        settings.unprompted_speech_enabled = false;
        assert_eq!(
            house_may_speak(Some(&settings), at(7, 0), false),
            UnpromptedSpeech::Refused(SpeechRefusal::NotEnabled)
        );
        assert_eq!(
            house_may_speak(None, at(7, 0), false),
            UnpromptedSpeech::Refused(SpeechRefusal::SettingsUnreadable)
        );
    }

    #[test]
    fn markdown_marks_and_pictures_are_not_spoken() {
        assert_eq!(
            plain("## A *big* `day`\n> 1. Read ![a cat](https://x/c.png)[the page](https://x/p)"),
            "A big day Read the page"
        );
    }
}
