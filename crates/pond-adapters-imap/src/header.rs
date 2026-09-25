//! Header decoding: an undecoded `=?UTF-8?B?...?=` subject is noise to people and retrieval.

use base64::Engine;

/// Decode RFC 2047 `B`/`Q` encoded-words; undecodable ones are left exactly as found.
pub fn decode_rfc2047(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    // RFC 2047: whitespace BETWEEN two encoded-words is not part of the text.
    let mut prev_was_encoded = false;

    while let Some(start) = rest.find("=?") {
        let (before, tail) = rest.split_at(start);
        if !(prev_was_encoded && before.trim().is_empty()) {
            out.push_str(before);
        }
        // Find `?=` only AFTER the encoding field: in `?Q?=E2…` the separator itself reads as `?=`.
        let after_marker = &tail[2..];
        let Some(charset_end) = after_marker.find('?') else {
            out.push_str(tail);
            return out;
        };
        let Some(enc_end) = after_marker[charset_end + 1..]
            .find('?')
            .map(|i| charset_end + 1 + i)
        else {
            out.push_str(tail);
            return out;
        };
        let Some(text_end) = after_marker[enc_end + 1..]
            .find("?=")
            .map(|i| enc_end + 1 + i)
        else {
            out.push_str(tail);
            return out;
        };
        let end = text_end + 2;
        let encoding = &after_marker[charset_end + 1..enc_end];
        let text = &after_marker[enc_end + 1..text_end];
        let decoded = match encoding.to_ascii_uppercase().as_str() {
            "B" => base64::engine::general_purpose::STANDARD
                .decode(text)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned()),
            "Q" => Some(decode_q(text)),
            _ => None,
        };
        match decoded {
            Some(text) => {
                out.push_str(&text);
                prev_was_encoded = true;
            }
            // Undecodable: keep verbatim; a subject somebody can squint at beats a blank one.
            None => {
                out.push_str(&tail[..end + 2]);
                prev_was_encoded = false;
            }
        }
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Quoted-printable as RFC 2047 uses it, where `_` means a space.
fn decode_q(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'_' => {
                out.push(b' ');
                i += 1;
            }
            b'=' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(decode_rfc2047("Dentist appointment"), "Dentist appointment");
    }

    #[test]
    fn base64_words_are_decoded() {
        assert_eq!(
            decode_rfc2047("=?UTF-8?B?SGFiYXJpIHlha28=?="),
            "Habari yako"
        );
    }

    #[test]
    fn quoted_printable_words_are_decoded_with_underscore_as_space() {
        assert_eq!(decode_rfc2047("=?utf-8?Q?Rent_due?="), "Rent due");
        assert_eq!(decode_rfc2047("=?utf-8?Q?caf=C3=A9?="), "café");
    }

    #[test]
    fn adjacent_encoded_words_rejoin_without_the_separating_space() {
        assert_eq!(
            decode_rfc2047("=?utf-8?Q?Habari?= =?utf-8?Q?_yako?="),
            "Habari yako"
        );
    }

    #[test]
    fn text_around_an_encoded_word_is_kept() {
        assert_eq!(
            decode_rfc2047("Re: =?utf-8?Q?caf=C3=A9?= tomorrow"),
            "Re: café tomorrow"
        );
    }

    #[test]
    fn text_that_starts_with_an_equals_sign_does_not_end_the_word_early() {
        assert_eq!(
            decode_rfc2047("=?UTF-8?Q?=E2=9A=A1_$10.5K,_robot_arms?="),
            "\u{26a1} $10.5K, robot arms"
        );
        // The base64 form of the same trap: payloads routinely end in `=`.
        assert_eq!(decode_rfc2047("=?UTF-8?B?4pqhIHRlc3Q=?="), "\u{26a1} test");
    }

    #[test]
    fn an_unreadable_encoded_word_is_left_verbatim() {
        assert_eq!(
            decode_rfc2047("=?utf-8?X?something?="),
            "=?utf-8?X?something?="
        );
        assert_eq!(decode_rfc2047("=?truncated"), "=?truncated");
    }
}
