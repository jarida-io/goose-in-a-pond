//! Per-turn limits for image attachments (phase F1). An image turn bypasses the retained KV
//! prompt-session cache so it pays a full prefill, and a 12 MP photo decodes to ~36 MB of RGB in
//! the ~1 GB an 8 GB Orin has spare: unbounded is an OOM, not just slow. Clients downscale to
//! 1024 px longest edge (`pond-desktop/src/lib/imageAttach.ts`); these caps are the backstop.

use super::message::ImageAttachment;

/// Maximum number of images accepted in a single chat turn.
///
/// Four keeps the vision encoder to a couple of seconds on the Orin and the added prompt tokens
/// inside the pinned 4096-token context. The video sampler uses the same frame cap.
pub const MAX_IMAGES_PER_TURN: usize = 4;

/// Maximum decoded (post-base64) size of a single image, in bytes.
///
/// 4 MiB of compressed JPEG/PNG is far more than a 1024 px-longest-edge image
/// needs (typically 100-400 KiB) but leaves room for a lossless PNG screenshot.
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Maximum decoded size of ALL images in one turn, in bytes.
///
/// Bounds peak transient allocation per request, so `MAX_IMAGES_PER_TURN` images at
/// `MAX_IMAGE_BYTES` each cannot combine into a 16 MiB spike.
pub const MAX_TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Request-body ceiling for the chat routes, in bytes. Axum's 2 MiB `DefaultBodyLimit` is below
/// a legal attachment set (base64 inflates by 4/3), so this is sized to keep every rejection in
/// [`validate_turn_images`] reachable, `TotalTooLarge` included; the ~22 MiB transient that
/// costs is bounded by the SSE semaphore.
pub const MAX_CHAT_BODY_BYTES: usize =
    (MAX_IMAGES_PER_TURN * MAX_IMAGE_BYTES) * 4 / 3 + 1024 * 1024;

// The backstop must sit above EVERY policy limit, or the policy never runs and the caller gets
// Axum's "length limit exceeded" instead of an actionable message. Compile-time rather than a
// test because it is a relationship between constants, so a violation should not build.
const _: () = assert!(MAX_CHAT_BODY_BYTES > MAX_TOTAL_IMAGE_BYTES * 4 / 3);
const _: () = assert!(MAX_CHAT_BODY_BYTES > (MAX_IMAGES_PER_TURN * MAX_IMAGE_BYTES) * 4 / 3);
// Axum's own default is 2 MiB, which is below a single legal image.
const _: () = assert!(MAX_CHAT_BODY_BYTES > 2 * 1024 * 1024);

/// MIME types the mtmd vision path can decode.
///
/// Kept explicit rather than accepting any `image/*`: an unsupported container
/// fails deep inside the engine with a much worse error than a 415 here.
pub const SUPPORTED_IMAGE_MIME_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/gif",
    "image/bmp",
];

/// MIME types the ENGINE can decode, a strict subset of [`SUPPORTED_IMAGE_MIME_TYPES`].
///
/// mtmd decodes with `stb_image` alone (`mtmd-helper.cpp`), and the vendored `stb_image.h` has
/// no WebP decoder; the video fallthrough that might have caught it is behind `MTMD_VIDEO`,
/// which the build never defines. So WebP is accepted at the API and must be re-encoded to one
/// of these before it reaches the engine: sent as-is it fails deep inside the turn as "Failed to
/// decode image", and because the picture then stays the newest in history, every later turn
/// in the conversation fails the same way.
pub const ENGINE_DECODABLE_IMAGE_TYPES: &[&str] =
    &["image/jpeg", "image/png", "image/gif", "image/bmp"];

/// Whether the engine can decode `mime_type` as sent, parameters and case ignored.
pub fn is_engine_decodable(mime_type: &str) -> bool {
    let mime = mime_type.trim().to_ascii_lowercase();
    let base = mime.split(';').next().unwrap_or("").trim();
    ENGINE_DECODABLE_IMAGE_TYPES.contains(&base)
}

/// Why a turn's image attachments were rejected.
///
/// Deliberately carries the offending numbers so the HTTP layer can render an
/// actionable message ("3.2 MB, limit is 4.0 MB") instead of "bad request".
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
    /// `true` when the right HTTP status is 413 Payload Too Large rather than
    /// 400/415. Lets the API layer pick a status without matching variants.
    pub fn is_too_large(&self) -> bool {
        matches!(
            self,
            Self::ImageTooLarge { .. } | Self::TotalTooLarge { .. } | Self::TooManyImages { .. }
        )
    }
}

/// Decoded byte length of a base64 payload, without decoding it.
///
/// Exact for well-formed base64 (3 bytes per 4 characters, each trailing `=` one byte less),
/// and whitespace is ignored. Rejects an oversized payload before a decode buffer is allocated.
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

/// Validate a turn's image attachments against the per-request limits.
///
/// Pure: no allocation beyond the error path, no decoding. Call this before the
/// request reaches the agent so an oversized payload is a 4xx and not an OOM.
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

/// File extension for a supported image MIME type, without the dot.
///
/// Used when persisting an attachment to disk (phase F2) so the stored file is
/// openable by a human debugging a session.
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
        // JPEG and anything that slipped past validation land here; a `.jpg`
        // that is really something else is still readable by every viewer that
        // sniffs magic bytes.
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
        // Three images just under the per-image cap pass individually but blow
        // the aggregate budget.
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

    /// What the engine decodes is a subset of what the API accepts, and WebP is the gap the API
    /// has to close by re-encoding.
    #[test]
    fn the_engine_decodes_everything_accepted_except_webp() {
        for m in ENGINE_DECODABLE_IMAGE_TYPES {
            assert!(SUPPORTED_IMAGE_MIME_TYPES.contains(m), "{m}");
        }
        let gap: Vec<&str> = SUPPORTED_IMAGE_MIME_TYPES
            .iter()
            .copied()
            .filter(|m| !ENGINE_DECODABLE_IMAGE_TYPES.contains(m))
            .collect();
        assert_eq!(gap, ["image/webp"]);
        assert!(is_engine_decodable("IMAGE/JPEG; charset=binary"));
        assert!(!is_engine_decodable("image/webp"));
        assert!(!is_engine_decodable("image/heic"));
    }
}
