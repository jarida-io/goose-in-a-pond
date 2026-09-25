//! Rule-based [`Redactor`] adapter; no model in the loop, so it is cheap enough to leave on.
//!
//! The regexes only find candidates; the validators in `pond_core::security::domain::redaction`
//! make every accept/reject decision.

use pond_core::security::domain::redaction::{
    candidate_is_real, Redacted, RedactionKind, RedactionLevel,
};
use pond_core::security::ports::redactor::Redactor;
use regex::Regex;

/// One permissive pattern per [`RedactionKind`]; `every_kind_has_a_pattern` enforces coverage.
const PATTERNS: &[(RedactionKind, &str)] = &[
    (
        RedactionKind::EmailAddress,
        r"(?i)[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,24}",
    ),
    (RedactionKind::PhoneNumber, r"\+?\d[\d \-()]{5,18}\d"),
    (
        RedactionKind::PostalCode,
        r"(?i)\b[A-Za-z]{1,2}\d[A-Za-z\d]?\s?\d[A-Za-z]{2}\b",
    ),
    (RedactionKind::PaymentCard, r"\b\d(?:[ \-]?\d){11,18}\b"),
    (
        RedactionKind::Iban,
        r"\b[A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]){11,30}\b",
    ),
    (RedactionKind::ApiKey, r"[A-Za-z0-9_\-]{8,200}"),
];

pub struct RuleRedactor {
    patterns: Vec<(RedactionKind, Regex)>,
}

impl RuleRedactor {
    pub fn new() -> Self {
        Self {
            patterns: PATTERNS
                .iter()
                .map(|(kind, pattern)| {
                    (
                        *kind,
                        Regex::new(pattern).expect("redaction pattern must compile"),
                    )
                })
                .collect(),
        }
    }

    /// Validated, non-overlapping spans, in order of appearance.
    fn spans(&self, text: &str) -> Vec<(RedactionKind, usize, usize)> {
        let mut found: Vec<(RedactionKind, usize, usize)> = Vec::new();
        for (kind, re) in &self.patterns {
            for m in re.find_iter(text) {
                let raw = m.as_str();
                let lead = raw.len() - raw.trim_start().len();
                let trail = raw.len() - raw.trim_end().len();
                let (start, end) = (m.start() + lead, m.end() - trail);
                if start >= end {
                    continue;
                }
                // Trimmed, or "call 020 7946 0958 now" would become "call[redacted:phone]now".
                if candidate_is_real(*kind, &text[start..end]) {
                    found.push((*kind, start, end));
                }
            }
        }
        // Overlaps: higher sensitivity wins, then the earlier match, then the longer one.
        found.sort_by(|a, b| {
            b.0.sensitivity()
                .cmp(&a.0.sensitivity())
                .then(a.1.cmp(&b.1))
                .then((b.2 - b.1).cmp(&(a.2 - a.1)))
        });
        let mut kept: Vec<(RedactionKind, usize, usize)> = Vec::new();
        for span in found {
            if kept.iter().any(|k| span.1 < k.2 && k.1 < span.2) {
                continue;
            }
            kept.push(span);
        }
        kept.sort_by_key(|s| s.1);
        kept
    }
}

impl Default for RuleRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor for RuleRedactor {
    fn redact(&self, text: &str, level: RedactionLevel) -> Redacted {
        let spans = self.spans(text);
        if spans.is_empty() {
            return Redacted::unchanged(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut findings = Vec::with_capacity(spans.len());
        let mut cursor = 0usize;
        for (kind, start, end) in spans {
            findings.push(kind);
            if !level.redacts(kind) {
                continue;
            }
            out.push_str(&text[cursor..start]);
            out.push_str(kind.placeholder());
            cursor = end;
        }
        out.push_str(&text[cursor..]);
        Redacted {
            text: out,
            findings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_pattern() {
        let r = RuleRedactor::new();
        for kind in RedactionKind::ALL {
            assert!(
                r.patterns.iter().any(|(k, _)| *k == kind),
                "no candidate pattern for {kind:?}; core can classify it and the \
                 adapter can never find it"
            );
        }
    }

    #[test]
    fn prose_around_a_match_survives_byte_for_byte() {
        let r = RuleRedactor::new();
        let out = r
            .redact(
                "I grew up near SW1A 1AA and moved away in 2005.",
                RedactionLevel::Full,
            )
            .text;
        assert_eq!(
            out,
            "I grew up near [redacted:postcode] and moved away in 2005."
        );

        let out = r
            .redact("call 020 7946 0958 now", RedactionLevel::Full)
            .text;
        assert_eq!(out, "call [redacted:phone] now");
    }

    #[test]
    fn ordinary_smart_home_prose_is_untouched() {
        let r = RuleRedactor::new();
        for sentence in [
            "The hub is at 192.168.1.50 on the study shelf.",
            "Play B2 3AM by the band when I get home.",
            "Firmware 10.0.19041.1234 shipped on 2026-08-05 and broke pairing.",
            "See commit 3f2a9c1d8b4e5f60718293a4b5c6d7e8f9012345 for the fix.",
            "The sensor MAC is 00-14-22-01-23-45.",
        ] {
            let out = r.redact(sentence, RedactionLevel::Full);
            assert_eq!(out.text, sentence, "mangled: {sentence}");
            assert!(out.findings.is_empty(), "false positive in: {sentence}");
        }
    }

    #[test]
    fn secrets_level_removes_a_credential_and_leaves_contact_details() {
        let r = RuleRedactor::new();
        let text = "mail jerry@example.com, key sk-abcdefghijklmnopqrstuvwxyz123456";
        let out = r.redact(text, RedactionLevel::Secrets);
        assert!(out.text.contains("jerry@example.com"), "{}", out.text);
        assert!(out.text.contains("[redacted:api-key]"), "{}", out.text);
        let detected = r.redact(text, RedactionLevel::Detect);
        assert_eq!(detected.text, text);
        assert_eq!(detected.findings, out.findings);
        assert!(detected.found(RedactionKind::EmailAddress));
    }

    #[test]
    fn redaction_is_idempotent() {
        let r = RuleRedactor::new();
        let text = "card 4111 1111 1111 1111, iban GB82 WEST 1234 5698 7654 32, \
                    mail liz@example.co.ke, home SW1A 1AA";
        let once = r.redact(text, RedactionLevel::Full).text;
        let twice = r.redact(&once, RedactionLevel::Full).text;
        assert_eq!(once, twice);
        assert!(!once.contains("4111"), "{once}");
        assert!(!once.contains("7654 32"), "{once}");
    }
}
