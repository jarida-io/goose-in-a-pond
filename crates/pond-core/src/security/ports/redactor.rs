//! Driven port: removal of personal data before it is stored or sent.
//! Never run it over the model's own prompt: it guards durable stores and egress only.

use crate::security::domain::redaction::{Redacted, RedactionLevel};

pub trait Redactor: Send + Sync {
    /// Replace detected personal data in `text`, reporting what was found; must be idempotent.
    fn redact(&self, text: &str, level: RedactionLevel) -> Redacted;
}
