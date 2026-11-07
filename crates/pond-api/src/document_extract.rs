//! A PDF's text layer, read before the turn is saved, on the blocking pool. A page that will not
//! parse is skipped; a document with no text on any page is a scan.

use lopdf::Document;
use pond_core::models::domain::document_input::{
    tidy_text, DocumentError, MAX_DOCUMENT_PAGES, MAX_DOCUMENT_TEXT_BYTES,
};

/// The file's text, tidied. `name` is the display name the errors quote.
pub fn extract_text(bytes: &[u8], name: &str) -> Result<String, DocumentError> {
    let named = || name.to_string();
    // The spec allows junk before the header within the first 1 KB; anything else is not a PDF.
    let head = &bytes[..bytes.len().min(1024)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(DocumentError::NotPdf { name: named() });
    }
    let mut doc = match Document::load_mem(bytes) {
        Ok(doc) => doc,
        Err(lopdf::Error::InvalidPassword | lopdf::Error::Decryption(_)) => {
            return Err(DocumentError::Encrypted { name: named() })
        }
        Err(_) => return Err(DocumentError::Unreadable { name: named() }),
    };
    // Permissions-only encryption opens with the empty password, as every reader does.
    if doc.is_encrypted() && doc.decrypt("").is_err() {
        return Err(DocumentError::Encrypted { name: named() });
    }

    let pages: Vec<u32> = doc
        .get_pages()
        .keys()
        .copied()
        .take(MAX_DOCUMENT_PAGES as usize)
        .collect();
    if pages.is_empty() {
        return Err(DocumentError::Unreadable { name: named() });
    }
    let mut raw = String::new();
    for page in pages {
        // Past the budget the model would never see it; the cap is applied again across documents.
        if raw.len() > MAX_DOCUMENT_TEXT_BYTES * 2 {
            break;
        }
        for chunk in doc.extract_text_chunks(&[page]).into_iter().flatten() {
            raw.push_str(&chunk);
        }
        raw.push('\n');
    }
    let text = tidy_text(&raw);
    if text.chars().filter(|c| c.is_alphanumeric()).count() < 3 {
        return Err(DocumentError::NoText { name: named() });
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!("../tests/fixtures/attachments/", $name)).as_slice()
        };
    }

    #[test]
    fn a_text_pdf_gives_its_words() {
        let text = extract_text(fixture!("text.pdf"), "shed.pdf").unwrap();
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(
                "The spare key to the shed is under the blue flowerpot by the back door."
            ),
            "{flat}"
        );
    }

    #[test]
    fn a_scan_has_no_text() {
        assert_eq!(
            extract_text(fixture!("scan.pdf"), "scan.pdf"),
            Err(DocumentError::NoText {
                name: "scan.pdf".into()
            })
        );
    }

    #[test]
    fn a_password_protected_pdf_is_refused() {
        assert_eq!(
            extract_text(fixture!("encrypted.pdf"), "locked.pdf"),
            Err(DocumentError::Encrypted {
                name: "locked.pdf".into()
            })
        );
    }

    #[test]
    fn broken_and_foreign_files_are_refused() {
        let pdf = fixture!("text.pdf");
        let err = extract_text(&pdf[..pdf.len() / 3], "cut.pdf").unwrap_err();
        assert!(
            matches!(
                err,
                DocumentError::Unreadable { .. } | DocumentError::NoText { .. }
            ),
            "{err:?}"
        );
        assert_eq!(
            extract_text(b"\x89PNG\r\n\x1a\n not a pdf", "photo.pdf"),
            Err(DocumentError::NotPdf {
                name: "photo.pdf".into()
            })
        );
        assert_eq!(
            extract_text(b"%PDF-1.7\n garbage with no objects", "junk.pdf").unwrap_err(),
            DocumentError::Unreadable {
                name: "junk.pdf".into()
            }
        );
    }
}
