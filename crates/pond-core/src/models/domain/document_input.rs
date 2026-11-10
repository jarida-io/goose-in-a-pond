//! PDFs a chat turn carries. The pond reads their text and gives it to the model after the typed
//! message as `name:` and a fenced block, the shape the phone already sends a text file in.

use super::audio_input::persisted_mark;
use super::image_limits::decoded_len;
use super::message::{AudioAttachment, DocumentAttachment};

pub const MAX_DOCUMENTS_PER_TURN: usize = 3;

/// Per file, as sent. A text PDF exported on a phone is rarely over 1 MB; scans are refused anyway.
pub const MAX_DOCUMENT_BYTES: usize = 5 * 1024 * 1024;

/// UTF-8 bytes of text the model gets per turn, across every document: about 6,000 tokens, which
/// the nano prefills in about 11 s on top of a 10k-token turn in its 64k window.
pub const MAX_DOCUMENT_TEXT_BYTES: usize = 24 * 1024;

/// Pages read from one document; the text budget is spent long before this on any real PDF.
pub const MAX_DOCUMENT_PAGES: u32 = 200;

pub const SUPPORTED_DOCUMENT_MIME_TYPES: &[&str] = &["application/pdf"];

const MAX_NAME_CHARS: usize = 120;

/// The name the model and the saved row see: the last path part, printable, at most 120 chars.
pub fn display_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = last
        .chars()
        .filter(|c| !c.is_control() && *c != '`')
        .take(MAX_NAME_CHARS)
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "document.pdf".to_string()
    } else {
        clean.to_string()
    }
}

/// One document's text as the model gets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentText {
    pub name: String,
    pub text: String,
    /// Bytes of text the document had, when the per-turn budget cut it.
    pub cut_from: Option<usize>,
}

/// Why a turn's documents were refused before anything was saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    TooMany {
        count: usize,
        max: usize,
    },
    TooLarge {
        name: String,
        bytes: usize,
        max: usize,
    },
    NotPdf {
        name: String,
    },
    Unreadable {
        name: String,
    },
    Encrypted {
        name: String,
    },
    NoText {
        name: String,
    },
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooMany { count, max } => write!(
                f,
                "Too many documents in one message: {count} attached, at most {max} per message."
            ),
            Self::TooLarge { name, bytes, max } => write!(
                f,
                "{name} is {:.1} MB, over the {:.0} MB limit for a document.",
                *bytes as f64 / 1_048_576.0,
                *max as f64 / 1_048_576.0
            ),
            Self::NotPdf { name } => write!(f, "{name} is not a PDF. Only PDFs can be read here."),
            Self::Unreadable { name } => write!(
                f,
                "{name} could not be read: the file is damaged or incomplete."
            ),
            Self::Encrypted { name } => write!(
                f,
                "{name} is password-protected. Remove the password and send it again."
            ),
            Self::NoText { name } => write!(
                f,
                "{name} has no text the pond can read: it looks like a scan or photos of pages. \
                 Send the pages as pictures instead."
            ),
        }
    }
}

impl std::error::Error for DocumentError {}

impl DocumentError {
    /// The API's `code` field.
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooMany { .. } | Self::TooLarge { .. } => "document_too_large",
            Self::NoText { .. } => "document_no_text",
            Self::NotPdf { .. } | Self::Unreadable { .. } | Self::Encrypted { .. } => {
                "document_invalid"
            }
        }
    }
}

/// Count, type and size, from the base64 alone: runs before anything is decoded.
pub fn validate_turn_documents(documents: &[DocumentAttachment]) -> Result<(), DocumentError> {
    if documents.len() > MAX_DOCUMENTS_PER_TURN {
        return Err(DocumentError::TooMany {
            count: documents.len(),
            max: MAX_DOCUMENTS_PER_TURN,
        });
    }
    for doc in documents {
        let name = display_name(&doc.name);
        let mime = doc.mime_type.trim().to_ascii_lowercase();
        let base = mime.split(';').next().unwrap_or("").trim();
        if !SUPPORTED_DOCUMENT_MIME_TYPES.contains(&base) {
            return Err(DocumentError::NotPdf { name });
        }
        let bytes = decoded_len(&doc.data);
        if bytes == 0 {
            return Err(DocumentError::Unreadable { name });
        }
        if bytes > MAX_DOCUMENT_BYTES {
            return Err(DocumentError::TooLarge {
                name,
                bytes,
                max: MAX_DOCUMENT_BYTES,
            });
        }
    }
    Ok(())
}

/// Trailing spaces off every line and at most one blank line in a row: PDF text is mostly layout.
pub fn tidy_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank_run = 0usize;
    for line in raw.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Spend the per-turn text budget across the documents in order, cutting at a char boundary.
pub fn fit_to_budget(extracted: Vec<(String, String)>) -> Vec<DocumentText> {
    let mut left = MAX_DOCUMENT_TEXT_BYTES;
    extracted
        .into_iter()
        .map(|(name, text)| {
            if text.len() <= left {
                left -= text.len();
                return DocumentText {
                    name,
                    text,
                    cut_from: None,
                };
            }
            let mut end = left;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            left -= end;
            DocumentText {
                name,
                cut_from: Some(text.len()),
                text: text[..end].to_string(),
            }
        })
        .collect()
}

/// A fence no line of the text can close early.
fn fence_for(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

/// The turn's text for the model: the typed message, then each document as `name:` and a fence.
pub fn compose_for_model(message: &str, documents: &[DocumentText]) -> String {
    let mut out = message.trim_end().to_string();
    for doc in documents {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&doc.name);
        out.push_str(":\n");
        if doc.text.is_empty() {
            out.push_str(
                "[Not included: this message already carries as much document text as the pond \
                 passes on in one turn.]",
            );
            continue;
        }
        let fence = fence_for(&doc.text);
        out.push_str(&format!("{fence}\n{}\n{fence}", doc.text));
        if let Some(total) = doc.cut_from {
            out.push_str(&format!(
                "\n[Only the first {} KB of this document's {} KB of text was included.]",
                doc.text.len().div_ceil(1024),
                total.div_ceil(1024)
            ));
        }
    }
    out
}

/// The text a user row is saved with: the typed message, then one mark per document and recording.
/// The pond keeps neither the files nor their text.
pub fn persisted_text(
    message: &str,
    audio: &[AudioAttachment],
    documents: &[DocumentAttachment],
) -> String {
    if audio.is_empty() && documents.is_empty() {
        return message.to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    let typed = message.trim();
    if !typed.is_empty() {
        lines.push(typed.to_string());
    }
    lines.extend(
        documents
            .iter()
            .map(|d| format!("[Document: {}]", display_name(&d.name))),
    );
    lines.extend(audio.iter().map(persisted_mark));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf(name: &str, data: &str) -> DocumentAttachment {
        DocumentAttachment {
            data: data.to_string(),
            mime_type: "application/pdf".to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn names_lose_paths_controls_and_backticks() {
        assert_eq!(display_name("/sdcard/Download/Lease.pdf"), "Lease.pdf");
        assert_eq!(display_name("C:\\docs\\a`b\u{7}.pdf"), "ab.pdf");
        assert_eq!(display_name("  "), "document.pdf");
        assert_eq!(display_name(&"x".repeat(500)).chars().count(), 120);
    }

    #[test]
    fn the_request_limits_count_type_and_size() {
        assert_eq!(validate_turn_documents(&[pdf("a.pdf", "JVBERi0=")]), Ok(()));
        let four = vec![pdf("a.pdf", "JVBERi0="); 4];
        assert_eq!(
            validate_turn_documents(&four).unwrap_err().code(),
            "document_too_large"
        );
        let mut word = pdf("notes.docx", "JVBERi0=");
        word.mime_type = "application/msword".to_string();
        assert_eq!(
            validate_turn_documents(&[word]).unwrap_err(),
            DocumentError::NotPdf {
                name: "notes.docx".to_string()
            }
        );
        assert_eq!(
            validate_turn_documents(&[pdf("e.pdf", "")])
                .unwrap_err()
                .code(),
            "document_invalid"
        );
        let big = "A".repeat((MAX_DOCUMENT_BYTES + 3) * 4 / 3);
        assert_eq!(
            validate_turn_documents(&[pdf("big.pdf", &big)])
                .unwrap_err()
                .code(),
            "document_too_large"
        );
    }

    #[test]
    fn tidy_text_keeps_one_blank_line() {
        assert_eq!(tidy_text("  a  \n\n\n\nb\t\n\n"), "a\n\nb");
    }

    #[test]
    fn the_budget_is_shared_in_order_and_cuts_on_a_char_boundary() {
        let first = "é".repeat(MAX_DOCUMENT_TEXT_BYTES / 2 - 1);
        let second = "ü".repeat(MAX_DOCUMENT_TEXT_BYTES);
        let fitted = fit_to_budget(vec![
            ("a.pdf".into(), first.clone()),
            ("b.pdf".into(), second.clone()),
            ("c.pdf".into(), "late".into()),
        ]);
        assert_eq!(fitted[0].text, first);
        assert_eq!(fitted[0].cut_from, None);
        assert_eq!(fitted[1].cut_from, Some(second.len()));
        assert_eq!(
            fitted[0].text.len() + fitted[1].text.len(),
            MAX_DOCUMENT_TEXT_BYTES
        );
        assert!(fitted[2].text.is_empty());
        assert_eq!(fitted[2].cut_from, Some(4));
    }

    #[test]
    fn the_model_gets_name_and_a_fence_after_the_typed_message() {
        let docs = vec![DocumentText {
            name: "shed.pdf".into(),
            text: "key under ``` the pot".into(),
            cut_from: None,
        }];
        assert_eq!(
            compose_for_model("where is the key?", &docs),
            "where is the key?\n\nshed.pdf:\n````\nkey under ``` the pot\n````"
        );
        assert_eq!(
            compose_for_model("", &docs),
            "shed.pdf:\n````\nkey under ``` the pot\n````"
        );
        let cut = vec![DocumentText {
            name: "long.pdf".into(),
            text: "x".repeat(2048),
            cut_from: Some(10_000),
        }];
        assert!(compose_for_model("", &cut)
            .ends_with("[Only the first 2 KB of this document's 10 KB of text was included.]"));
    }

    #[test]
    fn the_saved_row_marks_each_document_and_recording() {
        let docs = [pdf("Lease.pdf", "x"), pdf("", "x")];
        let note = [AudioAttachment {
            data: "x".into(),
            mime_type: "audio/wav".into(),
            name: None,
            heard_as: None,
        }];
        assert_eq!(
            persisted_text("  read these ", &note, &docs),
            "read these\n[Document: Lease.pdf]\n[Document: document.pdf]\n[Voice note]"
        );
        assert_eq!(persisted_text("", &note, &[]), "[Voice note]");
        assert_eq!(persisted_text("plain", &[], &[]), "plain");
    }
}
