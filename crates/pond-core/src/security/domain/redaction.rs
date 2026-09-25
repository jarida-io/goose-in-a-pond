//! Redaction policy. Regexes live in `pond_infra::rule_redactor`; each rule here validates its
//! deliberately over-permissive candidates so ordinary prose survives.

use serde::{Deserialize, Serialize};

use crate::security::domain::event::PrivacySensitivity;

/// A class of personal data the redactor can recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionKind {
    EmailAddress,
    PhoneNumber,
    PostalCode,
    PaymentCard,
    Iban,
    ApiKey,
}

impl RedactionKind {
    /// Every variant; keep it in step by hand when adding one.
    pub const ALL: [RedactionKind; 6] = [
        RedactionKind::EmailAddress,
        RedactionKind::PhoneNumber,
        RedactionKind::PostalCode,
        RedactionKind::PaymentCard,
        RedactionKind::Iban,
        RedactionKind::ApiKey,
    ];

    /// Stable, greppable name for logs. Never the matched text.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EmailAddress => "email",
            Self::PhoneNumber => "phone",
            Self::PostalCode => "postcode",
            Self::PaymentCard => "card",
            Self::Iban => "iban",
            Self::ApiKey => "api-key",
        }
    }

    /// Named, not blanked, so the text still reads and the user sees what was taken.
    pub fn placeholder(&self) -> &'static str {
        match self {
            Self::EmailAddress => "[redacted:email]",
            Self::PhoneNumber => "[redacted:phone]",
            Self::PostalCode => "[redacted:postcode]",
            Self::PaymentCard => "[redacted:card]",
            Self::Iban => "[redacted:iban]",
            Self::ApiKey => "[redacted:api-key]",
        }
    }

    /// How bad this is to leak, on the one ordering the codebase already has.
    pub fn sensitivity(&self) -> PrivacySensitivity {
        match self {
            Self::EmailAddress | Self::PhoneNumber | Self::PostalCode => {
                PrivacySensitivity::Sensitive
            }
            Self::PaymentCard | Self::Iban | Self::ApiKey => PrivacySensitivity::Secret,
        }
    }
}

/// How much a given chokepoint removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionLevel {
    /// Report findings, replace nothing. The audit-only posture.
    Detect,
    /// Replace only what classifies [`PrivacySensitivity::Secret`].
    Secrets,
    /// Replace everything the rules recognise.
    Full,
}

impl RedactionLevel {
    /// Derived from `sensitivity()`, so a new `Secret` kind can't survive at `Secrets` level.
    pub fn redacts(&self, kind: RedactionKind) -> bool {
        match self {
            Self::Detect => false,
            Self::Secrets => kind.sensitivity() == PrivacySensitivity::Secret,
            Self::Full => true,
        }
    }
}

/// The result of one redaction pass.
#[derive(Debug, Clone, PartialEq)]
pub struct Redacted {
    /// The text after replacement; the input unchanged at [`RedactionLevel::Detect`].
    pub text: String,
    /// What was found, in order of appearance, whether or not it was replaced.
    pub findings: Vec<RedactionKind>,
}

impl Redacted {
    /// Nothing matched.
    pub fn unchanged(text: &str) -> Self {
        Self {
            text: text.to_string(),
            findings: Vec::new(),
        }
    }

    pub fn found(&self, kind: RedactionKind) -> bool {
        self.findings.contains(&kind)
    }

    /// The sensitivity of the worst thing found, if anything was.
    pub fn highest_sensitivity(&self) -> Option<PrivacySensitivity> {
        self.findings.iter().map(|k| k.sensitivity()).max()
    }
}

// -- The rules -------------------------------------------------------------

/// Single exhaustive dispatch, so the adapter cannot forget a kind.
pub fn candidate_is_real(kind: RedactionKind, candidate: &str) -> bool {
    match kind {
        RedactionKind::EmailAddress => is_email_shaped(candidate),
        RedactionKind::PhoneNumber => is_plausible_phone(candidate),
        RedactionKind::PostalCode => is_uk_postcode(candidate),
        RedactionKind::PaymentCard => is_payment_card(candidate),
        RedactionKind::Iban => passes_iban_checksum(candidate),
        RedactionKind::ApiKey => is_api_key_shaped(candidate),
    }
}

/// Local part, dotted domain, alphabetic TLD: rejects `user@localhost` and a trailing full stop.
pub fn is_email_shaped(candidate: &str) -> bool {
    let mut parts = candidate.split('@');
    let local = match parts.next() {
        Some(l) => l,
        None => return false,
    };
    let domain = match parts.next() {
        Some(d) => d,
        None => return false,
    };
    if parts.next().is_some() {
        return false;
    }
    if local.is_empty() || local.len() > 64 {
        return false;
    }
    if !local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
    {
        return false;
    }
    if domain.starts_with('.') || domain.ends_with('.') || !domain.contains('.') {
        return false;
    }
    let tld = domain.rsplit('.').next().unwrap_or("");
    tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// No `.`: on a smart-home pond, IP addresses far outnumber dot-separated phone numbers.
const PHONE_SEPARATORS: [char; 4] = [' ', '-', '(', ')'];

pub fn is_plausible_phone(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    let (international, rest) = match trimmed.strip_prefix('+') {
        Some(r) => (true, r),
        None => (false, trimmed),
    };
    if rest.is_empty() {
        return false;
    }
    if !rest
        .chars()
        .all(|c| c.is_ascii_digit() || PHONE_SEPARATORS.contains(&c))
    {
        return false;
    }
    if starts_like_an_iso_date(rest) {
        return false;
    }
    let digits = rest.chars().filter(|c| c.is_ascii_digit()).count();
    let groups: Vec<&str> = rest
        .split(|c| PHONE_SEPARATORS.contains(&c))
        .filter(|g| !g.is_empty())
        .collect();
    // A MAC address or a hyphenated serial: five or more groups of exactly two.
    if groups.len() >= 5 && groups.iter().all(|g| g.len() == 2) {
        return false;
    }
    if international {
        return (8..=15).contains(&digits);
    }
    if groups.len() > 1 {
        return (9..=15).contains(&digits);
    }
    // A bare run needs a trunk prefix, or epoch timestamps (10 and 13 digits) read as phones.
    rest.starts_with('0') && (10..=11).contains(&digits)
}

fn starts_like_an_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// Letters a real UK inward code can use. C, I, K, M, O and V never appear.
const UK_INWARD_LETTERS: [char; 20] = [
    'A', 'B', 'D', 'E', 'F', 'G', 'H', 'J', 'L', 'N', 'P', 'Q', 'R', 'S', 'T', 'U', 'W', 'X', 'Y',
    'Z',
];
const UK_AREA_FIRST_EXCLUDED: [char; 3] = ['Q', 'V', 'X'];
const UK_AREA_SECOND_EXCLUDED: [char; 3] = ['I', 'J', 'Z'];

/// Structural UK postcode check: the inward-letter set rejects prose like "B2 3AM".
pub fn is_uk_postcode(candidate: &str) -> bool {
    let compact: Vec<char> = candidate
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if compact.len() < 5 || compact.len() > 7 {
        return false;
    }
    if !compact.iter().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let (outward, inward) = compact.split_at(compact.len() - 3);
    if !inward[0].is_ascii_digit() {
        return false;
    }
    if !inward[1..].iter().all(|c| UK_INWARD_LETTERS.contains(c)) {
        return false;
    }
    if !outward[0].is_ascii_alphabetic() || UK_AREA_FIRST_EXCLUDED.contains(&outward[0]) {
        return false;
    }
    let area_len = if outward[1].is_ascii_alphabetic() {
        2
    } else {
        1
    };
    if area_len == 2 && UK_AREA_SECOND_EXCLUDED.contains(&outward[1]) {
        return false;
    }
    let district = &outward[area_len..];
    if district.is_empty() || district.len() > 2 {
        return false;
    }
    if !district[0].is_ascii_digit() {
        return false;
    }
    district.len() == 1 || district[1].is_ascii_alphanumeric()
}

/// Luhn is the whole rule; without it every 16-digit order number or serial reads as a card.
pub fn passes_luhn(digits: &str) -> bool {
    let mut sum: u32 = 0;
    let mut alt = false;
    let mut count = 0usize;
    for c in digits.chars().rev() {
        let d = match c.to_digit(10) {
            Some(d) => d,
            None => return false,
        };
        count += 1;
        let v = if alt {
            let x = d * 2;
            if x > 9 {
                x - 9
            } else {
                x
            }
        } else {
            d
        };
        sum += v;
        alt = !alt;
    }
    (12..=19).contains(&count) && sum % 10 == 0
}

pub fn is_payment_card(candidate: &str) -> bool {
    if !candidate
        .chars()
        .all(|c| c.is_ascii_digit() || c == ' ' || c == '-')
    {
        return false;
    }
    let digits: String = candidate.chars().filter(|c| c.is_ascii_digit()).collect();
    passes_luhn(&digits)
}

/// ISO 13616 mod-97. Same argument as Luhn: the shape alone is far too common.
pub fn passes_iban_checksum(candidate: &str) -> bool {
    let chars: Vec<char> = candidate
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if chars.len() < 15 || chars.len() > 34 {
        return false;
    }
    if !chars.iter().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    if !chars[0].is_ascii_alphabetic() || !chars[1].is_ascii_alphabetic() {
        return false;
    }
    if !chars[2].is_ascii_digit() || !chars[3].is_ascii_digit() {
        return false;
    }
    let mut remainder: u32 = 0;
    for c in chars[4..].iter().chain(chars[..4].iter()) {
        match c.to_digit(10) {
            Some(d) => remainder = (remainder * 10 + d) % 97,
            None => {
                let v = (*c as u32) - ('A' as u32) + 10;
                remainder = (remainder * 100 + v) % 97;
            }
        }
    }
    remainder == 1
}

/// Known credential prefixes: the only cheap rule that spares git SHAs, UUIDs and long words.
const API_KEY_PREFIXES: &[&str] = &[
    "sk-",
    "sk_live_",
    "sk_test_",
    "pk_live_",
    "rk_live_",
    "shpat_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xapp-",
    "AKIA",
    "ASIA",
    "AIza",
    "hf_",
    "npm_",
];

pub fn is_api_key_shaped(candidate: &str) -> bool {
    if !candidate
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return false;
    }
    if let Some(prefix) = API_KEY_PREFIXES.iter().find(|p| candidate.starts_with(**p)) {
        let body_len = candidate.len() - prefix.len();
        if body_len >= 12 {
            return true;
        }
    }
    is_high_entropy_secret(candidate)
}

/// Deliberately narrow unprefixed fallback: requiring all three cases plus non-hex spares git
/// SHAs and UUIDs (hex), long words (no digit) and base32 tokens (no lower case).
fn is_high_entropy_secret(t: &str) -> bool {
    if t.len() < 32 || t.len() > 128 {
        return false;
    }
    let has_upper = t.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = t.chars().any(|c| c.is_ascii_lowercase());
    let has_digit = t.chars().any(|c| c.is_ascii_digit());
    let is_hex = t.chars().all(|c| c.is_ascii_hexdigit());
    has_upper && has_lower && has_digit && !is_hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_kind_and_the_level_derives_from_sensitivity() {
        assert_eq!(RedactionKind::ALL.len(), 6);
        for kind in RedactionKind::ALL {
            assert!(!RedactionLevel::Detect.redacts(kind));
            assert!(RedactionLevel::Full.redacts(kind));
            assert_eq!(
                RedactionLevel::Secrets.redacts(kind),
                kind.sensitivity() == PrivacySensitivity::Secret
            );
        }
    }

    #[test]
    fn an_address_is_an_address_and_a_bare_host_is_not() {
        assert!(is_email_shaped("jerry@example.com"));
        assert!(is_email_shaped("liz.w+giap@sub.example.co.ke"));
        // A sentence's full stop is not part of the domain.
        assert!(!is_email_shaped("jerry@example.com."));
        // The pond's own services are addressed like this all day.
        assert!(!is_email_shaped("ollama@localhost"));
        assert!(!is_email_shaped("@channel"));
    }

    #[test]
    fn a_phone_number_is_a_phone_number() {
        assert!(is_plausible_phone("020 7946 0958"));
        assert!(is_plausible_phone("+44 20 7946 0958"));
        assert!(is_plausible_phone("07700900123"));
    }

    /// The three that actually appear in a smart-home pond's memories.
    #[test]
    fn a_date_followed_by_a_count_is_not_a_phone_number() {
        assert!(!is_plausible_phone("2026-08-05 12"));
        // An epoch timestamp: eleven to thirteen digits, no trunk prefix.
        assert!(!is_plausible_phone("1754400000000"));
        // A MAC address, which is exactly what a hub registration carries.
        assert!(!is_plausible_phone("00-14-22-01-23-45"));
    }

    #[test]
    fn a_uk_postcode_is_a_uk_postcode() {
        for good in [
            "SW1A 1AA", "EC1A 1BB", "M1 1AE", "B33 8TH", "DN55 1PT", "cr2 6xh",
        ] {
            assert!(is_uk_postcode(good), "{good} should be a postcode");
        }
    }

    /// Each matches a loose regex: no inward code uses C/I/K/M/O/V and Q/V/X never start an area.
    #[test]
    fn a_postcode_shaped_phrase_in_prose_is_not_a_postcode() {
        for bad in ["B2 3AM", "BA1 2AM", "A1 2CV", "V1 2AB", "S1 2IJ"] {
            assert!(!is_uk_postcode(bad), "{bad} must not read as a postcode");
        }
    }

    #[test]
    fn a_card_number_passes_luhn() {
        assert!(is_payment_card("4111 1111 1111 1111"));
        assert!(is_payment_card("4111-1111-1111-1111"));
    }

    #[test]
    fn an_ordinary_16_digit_number_is_not_a_card() {
        // Sequential digits: the classic order-number shape, fails Luhn.
        assert!(!is_payment_card("1234 5678 9012 3456"));
        // A thirteen-digit millisecond timestamp.
        assert!(!is_payment_card("1754400000000"));
    }

    #[test]
    fn an_iban_passes_mod_97_and_a_one_digit_edit_does_not() {
        assert!(passes_iban_checksum("GB82 WEST 1234 5698 7654 32"));
        assert!(passes_iban_checksum("GB82WEST12345698765432"));
        // Same shape, last digit changed. Shape alone would accept it.
        assert!(!passes_iban_checksum("GB82 WEST 1234 5698 7654 31"));
    }

    #[test]
    fn a_credential_is_a_credential() {
        assert!(is_api_key_shaped("sk-abcdefghijklmnopqrstuvwxyz123456"));
        assert!(is_api_key_shaped(
            "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"
        ));
        assert!(is_api_key_shaped("AKIAIOSFODNN7EXAMPLE"));
    }

    /// A rule that eats a commit SHA is a rule the user turns off.
    #[test]
    fn an_ordinary_identifier_is_not_a_credential() {
        // A 40-character git SHA: long, alphanumeric, and entirely hex.
        assert!(!is_api_key_shaped(
            "3f2a9c1d8b4e5f60718293a4b5c6d7e8f9012345"
        ));
        // A UUID with its hyphens stripped by the tokenizer.
        assert!(!is_api_key_shaped("550e8400e29b41d4a716446655440000"));
        // A long ordinary word: no digit.
        assert!(!is_api_key_shaped("supercalifragilisticexpialidocious"));
        // The prefix on its own, as it appears in prose: "rotate the AKIA key".
        assert!(!is_api_key_shaped("AKIA"));
    }

    #[test]
    fn candidate_is_real_dispatches_every_kind() {
        assert!(candidate_is_real(
            RedactionKind::EmailAddress,
            "jerry@example.com"
        ));
        assert!(!candidate_is_real(RedactionKind::PostalCode, "B2 3AM"));
    }
}
