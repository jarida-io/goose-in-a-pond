//! Per-turn image attachment limits: a 12 MP photo decodes to ~36 MB of RGB in the ~1 GB an
//! Orin has spare. Clients downscale to 1024 px longest edge; these caps are the backstop.

use super::message::ImageAttachment;

/// Max images per turn, and the video frame cap: fits the Orin's time and 4096-token budgets.
pub const MAX_IMAGES_PER_TURN: usize = 4;

/// Max decoded size of one image; a 1024 px JPEG is ~100-400 KiB, but PNG screenshots need room.
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Max decoded size of ALL images in a turn, so four max-size images can't make a 16 MiB spike.
pub const MAX_TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Chat-route body ceiling, above every image limit after base64's 4/3 so each rejection in
/// [`validate_turn_images`] is reachable; the ~22 MiB transient is bounded by the SSE semaphore.
pub const MAX_CHAT_BODY_BYTES: usize =
    (MAX_IMAGES_PER_TURN * MAX_IMAGE_BYTES) * 4 / 3 + 1024 * 1024;

// The body ceiling must exceed every policy limit, or Axum rejects before the policy runs.
const _: () = assert!(MAX_CHAT_BODY_BYTES > MAX_TOTAL_IMAGE_BYTES * 4 / 3);
const _: () = assert!(MAX_CHAT_BODY_BYTES > (MAX_IMAGES_PER_TURN * MAX_IMAGE_BYTES) * 4 / 3);
// Axum's own default is 2 MiB, which is below a single legal image.
const _: () = assert!(MAX_CHAT_BODY_BYTES > 2 * 1024 * 1024);

/// MIME types mtmd can decode; explicit because others fail deep in the engine, not as a 415.
pub const SUPPORTED_IMAGE_MIME_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/gif",
    "image/bmp",
];

/// Why a turn's images were rejected, with the numbers for an actionable HTTP message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageLimitError {
    TooManyImages {
        count: usize,
        max: usize,
    },
    ImageTooLarge {
        index: usize,
        bytes: usize,
        max: usize,
    },
    TotalTooLarge {
        bytes: usize,
        max: usize,
    },
    UnsupportedMimeType {
        index: usize,
        mime_type: String,
    },
    EmptyImage {
        index: usize,
    },
}

impl std::fmt::Display for ImageLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyImages { count, max } => write!(
                f,
                "too many images in one message: {count} attached, at most {max} are supported per turn"
            ),
            Self::ImageTooLarge { index, bytes, max } => write!(
                f,
                "image {} is {:.1} MB, which exceeds the {:.1} MB per-image limit — resize it before sending",
                index + 1,
                *bytes as f64 / (1024.0 * 1024.0),
                *max as f64 / (1024.0 * 1024.0),
            ),
            Self::TotalTooLarge { bytes, max } => write!(
                f,
                "attachments total {:.1} MB, which exceeds the {:.1} MB limit for one message",
                *bytes as f64 / (1024.0 * 1024.0),
                *max as f64 / (1024.0 * 1024.0),
            ),
            Self::UnsupportedMimeType { index, mime_type } => write!(
                f,
                "image {} has unsupported type \"{}\" — supported types are {}",
                index + 1,
                mime_type,
                SUPPORTED_IMAGE_MIME_TYPES.join(", ")
            ),
            Self::EmptyImage { index } => {
                write!(f, "image {} carries no data", index + 1)
            }
        }
    }
}

impl std::error::Error for ImageLimitError {}

impl ImageLimitError {
    /// `true` when the right status is 413 Payload Too Large rather than 400/415.
    pub fn is_too_large(&self) -> bool {
        matches!(
            self,
            Self::ImageTooLarge { .. } | Self::TotalTooLarge { .. } | Self::TooManyImages { .. }
        )
    }
}

/// Exact decoded length of well-formed base64 (whitespace ignored), without decoding it.
#[must_use]
pub fn decoded_len(base64: &str) -> usize {
    let mut chars = 0usize;
    let mut padding = 0usize;
    for b in base64.bytes() {
        match b {
            b' ' | b'\n' | b'\r' | b'\t' => {}
            b'=' => {
                chars += 1;
                padding += 1;
            }
            _ => chars += 1,
        }
    }
    // Unpadded base64 (some clients omit `=`): 2 trailing chars -> 1 byte, 3 -> 2.
    let remainder = match chars % 4 {
        2 => 1,
        3 => 2,
        _ => 0,
    };
    ((chars / 4) * 3 + remainder).saturating_sub(padding.min(2))
}

/// Validate a turn's images before they reach the agent, so oversize is a 4xx, not an OOM.
pub fn validate_turn_images(images: &[ImageAttachment]) -> Result<(), ImageLimitError> {
    if images.len() > MAX_IMAGES_PER_TURN {
        return Err(ImageLimitError::TooManyImages {
            count: images.len(),
            max: MAX_IMAGES_PER_TURN,
        });
    }

    let mut total = 0usize;
    for (index, img) in images.iter().enumerate() {
        let mime = img.mime_type.trim().to_ascii_lowercase();
        // Tolerate a charset/parameter suffix, e.g. "image/jpeg; charset=binary".
        let base = mime.split(';').next().unwrap_or("").trim();
        if !SUPPORTED_IMAGE_MIME_TYPES.contains(&base) {
            return Err(ImageLimitError::UnsupportedMimeType {
                index,
                mime_type: img.mime_type.clone(),
            });
        }

        let bytes = decoded_len(&img.data);
        if bytes == 0 {
            return Err(ImageLimitError::EmptyImage { index });
        }
        if bytes > MAX_IMAGE_BYTES {
            return Err(ImageLimitError::ImageTooLarge {
                index,
                bytes,
                max: MAX_IMAGE_BYTES,
            });
        }
        total = total.saturating_add(bytes);
    }

    if total > MAX_TOTAL_IMAGE_BYTES {
        return Err(ImageLimitError::TotalTooLarge {
            bytes: total,
            max: MAX_TOTAL_IMAGE_BYTES,
        });
    }

    Ok(())
}

/// File extension (no dot) for a supported image MIME type.
#[must_use]
pub fn extension_for_mime(mime_type: &str) -> &'static str {
    match mime_type
        .trim()
        .to_ascii_lowercase()
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
    {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        // Also catches anything that slipped past validation; viewers sniff magic bytes anyway.
        _ => "jpg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(bytes: usize, mime: &str) -> ImageAttachment {
        // 4 base64 chars per 3 bytes, rounded up, then padded to a multiple of 4.
        let groups = bytes.div_ceil(3);
        ImageAttachment {
            data: "A".repeat(groups * 4),
            mime_type: mime.to_string(),
        }
    }

    #[test]
    fn decoded_len_matches_real_base64_lengths() {
        // "hello" -> "aGVsbG8=" (5 bytes, one pad)
        assert_eq!(decoded_len("aGVsbG8="), 5);
        // "hell"  -> "aGVsbA==" (4 bytes, two pads)
        assert_eq!(decoded_len("aGVsbA=="), 4);
        // "hel"   -> "aGVs"     (3 bytes, no pad)
        assert_eq!(decoded_len("aGVs"), 3);
        assert_eq!(decoded_len(""), 0);
    }

    #[test]
    fn decoded_len_ignores_wrapping_whitespace() {
        assert_eq!(decoded_len("aGVs\naGVs\n"), 6);
    }

    #[test]
    fn decoded_len_handles_unpadded_input() {
        assert_eq!(decoded_len("aGVsbG8"), 5);
    }

    #[test]
    fn no_images_is_valid() {
        assert!(validate_turn_images(&[]).is_ok());
    }

    #[test]
    fn max_images_is_accepted_and_one_more_is_not() {
        let ok: Vec<_> = (0..MAX_IMAGES_PER_TURN)
            .map(|_| img(1024, "image/jpeg"))
            .collect();
        assert!(validate_turn_images(&ok).is_ok());

        let too_many: Vec<_> = (0..MAX_IMAGES_PER_TURN + 1)
            .map(|_| img(1024, "image/jpeg"))
            .collect();
        assert_eq!(
            validate_turn_images(&too_many),
            Err(ImageLimitError::TooManyImages {
                count: MAX_IMAGES_PER_TURN + 1,
                max: MAX_IMAGES_PER_TURN,
            })
        );
    }

    #[test]
    fn oversized_single_image_is_rejected_with_its_index() {
        let images = vec![
            img(1024, "image/png"),
            img(MAX_IMAGE_BYTES + 4096, "image/png"),
        ];
        match validate_turn_images(&images) {
            Err(ImageLimitError::ImageTooLarge { index, bytes, max }) => {
                assert_eq!(index, 1);
                assert!(bytes > MAX_IMAGE_BYTES);
                assert_eq!(max, MAX_IMAGE_BYTES);
            }
            other => panic!("expected ImageTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn total_budget_rejects_several_individually_legal_images() {
        let each = MAX_IMAGE_BYTES - 1024;
        let images: Vec<_> = (0..3).map(|_| img(each, "image/jpeg")).collect();
        for one in &images {
            assert!(validate_turn_images(std::slice::from_ref(one)).is_ok());
        }
        match validate_turn_images(&images) {
            Err(ImageLimitError::TotalTooLarge { max, .. }) => {
                assert_eq!(max, MAX_TOTAL_IMAGE_BYTES)
            }
            other => panic!("expected TotalTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_and_empty_payloads_are_rejected() {
        assert_eq!(
            validate_turn_images(&[img(64, "application/pdf")]),
            Err(ImageLimitError::UnsupportedMimeType {
                index: 0,
                mime_type: "application/pdf".to_string(),
            })
        );
        assert_eq!(
            validate_turn_images(&[ImageAttachment {
                data: String::new(),
                mime_type: "image/png".to_string(),
            }]),
            Err(ImageLimitError::EmptyImage { index: 0 })
        );
    }

    #[test]
    fn mime_type_matching_is_case_and_parameter_tolerant() {
        assert!(validate_turn_images(&[img(64, "IMAGE/JPEG")]).is_ok());
        assert!(validate_turn_images(&[img(64, "image/png; charset=binary")]).is_ok());
    }

    #[test]
    fn size_errors_map_to_payload_too_large() {
        assert!(ImageLimitError::TooManyImages { count: 9, max: 4 }.is_too_large());
        assert!(!ImageLimitError::UnsupportedMimeType {
            index: 0,
            mime_type: "x".into()
        }
        .is_too_large());
    }

    #[test]
    fn extensions_cover_every_supported_mime() {
        assert_eq!(extension_for_mime("image/png"), "png");
        assert_eq!(extension_for_mime("image/webp"), "webp");
        assert_eq!(extension_for_mime("image/gif"), "gif");
        assert_eq!(extension_for_mime("image/bmp"), "bmp");
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
        assert_eq!(extension_for_mime("IMAGE/PNG; q=1"), "png");
    }
}
