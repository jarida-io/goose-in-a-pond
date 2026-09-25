//! Pictures the engine can decode, whatever the client sent (design_v2 F.1).
//!
//! mtmd decodes with `stb_image` alone, which has no WebP decoder
//! (`image_limits::ENGINE_DECODABLE_IMAGE_TYPES`). The desktop re-encodes WebP before it uploads,
//! but a phone browser, an older client or a script may not, and the declared mime proves nothing:
//! a WebP labelled `image/jpeg` passes `validate_turn_images`, which reads only the label. A picture
//! the engine cannot read does not fail once. It stays the newest image in the history and fails
//! every later turn of the conversation, so it has to be caught before the turn is persisted.
//!
//! So each attachment's leading bytes are sniffed. JPEG, PNG, GIF and BMP pass untouched (their
//! label is corrected if it disagrees with the bytes); WebP is decoded under hard limits, off the
//! executor, and re-encoded to PNG when it has transparency and JPEG otherwise; anything else is
//! refused, because the engine could not read it either.

use base64::Engine as _;
use pond_core::models::domain::image_limits::MAX_IMAGE_BYTES;
use pond_core::models::domain::message::ImageAttachment;

/// Widest and tallest picture the WebP decoder accepts. Four times the 1024 px longest edge the
/// desktop sends, so only a picture no client of ours produces is refused.
pub(crate) const MAX_DECODE_EDGE_PX: u32 = 4096;

/// Allocation ceiling for one decode: exactly one 4096 x 4096 RGBA frame. A 4 MiB WebP may declare
/// 16383 x 16383, about 1 GiB decoded, which is an OOM on the Orin rather than a slow turn.
pub(crate) const MAX_DECODE_ALLOC_BYTES: u64 = 64 << 20;

/// JPEG quality for a re-encoded picture. The desktop's canvas re-encode uses 0.85
/// (`pond-desktop/src/lib/imageAttach.ts`), so a picture reads the same whichever side converted it.
pub(crate) const TRANSCODE_JPEG_QUALITY: u8 = 85;

/// Base64 characters enough to identify every container below: 16 characters are 12 bytes, which
/// covers the longest magic (`RIFF` + size + `WEBP`).
const SNIFF_BASE64_CHARS: usize = 16;

/// A container the leading bytes identify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Container {
    Jpeg,
    Png,
    Gif,
    Bmp,
    WebP,
}

impl Container {
    /// The label the engine and the attachment store should see.
    pub(crate) fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
            Self::WebP => "image/webp",
        }
    }
}

/// What `head` (the first bytes of a picture) says it is, by magic number alone.
pub(crate) fn sniff(head: &[u8]) -> Option<Container> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Container::Jpeg)
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Container::Png)
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some(Container::Gif)
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some(Container::WebP)
    } else if head.starts_with(b"BM") {
        Some(Container::Bmp)
    } else {
        None
    }
}

/// A picture that could not be read. `index` is zero-based; the household copy counts from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unreadable {
    pub index: usize,
}

/// Normalise a turn's pictures for the engine. Already-decodable pictures cost a 16-character
/// base64 decode each and never leave the executor; only a WebP pays for a decode, and that runs
/// in `spawn_blocking`. The result is in the same order as `images`.
pub(crate) async fn normalize_images_for_engine(
    mut images: Vec<ImageAttachment>,
) -> Result<Vec<ImageAttachment>, Unreadable> {
    let mut webp = Vec::new();
    for (index, img) in images.iter_mut().enumerate() {
        match sniff(&head_bytes(payload(&img.data)).ok_or(Unreadable { index })?) {
            None => return Err(Unreadable { index }),
            Some(Container::WebP) => webp.push(index),
            Some(container) => relabel(img, container),
        }
    }
    if webp.is_empty() {
        return Ok(images);
    }
    tokio::task::spawn_blocking(move || {
        for index in webp {
            let converted = std::panic::catch_unwind(|| transcode_webp(&images[index]))
                .ok()
                .flatten()
                .ok_or(Unreadable { index })?;
            images[index] = converted;
        }
        Ok(images)
    })
    .await
    // A panic is caught per picture above, so a join error means the runtime is shutting down;
    // nothing was persisted either way, and the first picture is as honest a pointer as any.
    .unwrap_or(Err(Unreadable { index: 0 }))
}

/// The base64 payload itself: a data-URL prefix and surrounding whitespace dropped, the same
/// tolerance the attachment store applies when it writes the picture to disk.
fn payload(data: &str) -> &str {
    data.rsplit_once("base64,")
        .map(|(_, b)| b)
        .unwrap_or(data)
        .trim()
}

/// Padding optional, as `image_limits::decoded_len` already allows.
fn lenient_base64() -> base64::engine::GeneralPurpose {
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    )
}

/// The first bytes of the picture, decoded from its first [`SNIFF_BASE64_CHARS`] characters
/// (inner whitespace skipped). `None` when those characters are not base64.
fn head_bytes(b64: &str) -> Option<Vec<u8>> {
    let head: String = b64
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .take(SNIFF_BASE64_CHARS)
        .collect();
    // Four characters decode independently of what follows, so a whole number of quads is a
    // valid decode of a prefix. A payload shorter than that is decoded as it stands.
    let usable = if head.len() >= 4 {
        head.len() - head.len() % 4
    } else {
        head.len()
    };
    lenient_base64().decode(&head[..usable]).ok()
}

/// The whole picture, decoded.
fn full_bytes(b64: &str) -> Option<Vec<u8>> {
    if b64.bytes().any(|b| b.is_ascii_whitespace()) {
        let compact: String = b64.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        lenient_base64().decode(compact).ok()
    } else {
        lenient_base64().decode(b64).ok()
    }
}

/// Correct a label that disagrees with the bytes. The bytes are what the engine will decode, and
/// the label is what names the stored file.
fn relabel(img: &mut ImageAttachment, container: Container) {
    let declared = img.mime_type.trim().to_ascii_lowercase();
    let base = declared.split(';').next().unwrap_or("").trim();
    if base != container.mime() {
        tracing::debug!(
            target: "giap::vision",
            declared = %img.mime_type,
            actual = container.mime(),
            "picture label corrected to match its bytes"
        );
        img.mime_type = container.mime().to_string();
    }
}

fn decode_limits() -> image::Limits {
    // `Limits` is non-exhaustive, so it is built from the default and narrowed.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE_PX);
    limits.max_image_height = Some(MAX_DECODE_EDGE_PX);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    limits
}

/// Decode one WebP under [`decode_limits`] and re-encode it for the engine. `None` when the bytes
/// do not decode, exceed a limit, or will not encode.
fn transcode_webp(img: &ImageAttachment) -> Option<ImageAttachment> {
    let bytes = full_bytes(payload(&img.data))?;
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::WebP);
    reader.limits(decode_limits());
    let decoded = reader.decode().ok()?;
    let (width, height) = (decoded.width(), decoded.height());
    let (encoded, mime) = encode_for_engine(decoded)?;
    tracing::info!(
        target: "giap::vision",
        width,
        height,
        from_bytes = bytes.len(),
        to_bytes = encoded.len(),
        to = mime,
        "re-encoded a WebP picture the engine cannot decode"
    );
    Some(ImageAttachment {
        data: base64::engine::general_purpose::STANDARD.encode(&encoded),
        mime_type: mime.to_string(),
    })
}

/// PNG when the picture really is transparent somewhere, JPEG otherwise. A transparent PNG that
/// would exceed the per-picture limit falls back to JPEG over white, as the desktop does, rather
/// than turning a legal 4 MB upload into a 413 about a file the household never made.
fn encode_for_engine(decoded: image::DynamicImage) -> Option<(Vec<u8>, &'static str)> {
    if decoded.color().has_alpha() {
        let rgba = decoded.into_rgba8();
        if rgba.pixels().any(|p| p.0[3] != u8::MAX) {
            let png = encode_png(&rgba)?;
            if png.len() <= MAX_IMAGE_BYTES {
                return Some((png, "image/png"));
            }
        }
        let rgb = flatten_onto_white(rgba)?;
        return encode_jpeg(&rgb).map(|j| (j, "image/jpeg"));
    }
    encode_jpeg(&decoded.into_rgb8()).map(|j| (j, "image/jpeg"))
}

/// Composite RGBA over white into RGB, reusing the RGBA buffer: a 4096 px frame is 64 MiB, and a
/// second buffer beside it would double the transient on a device with little to spare. Pixel `i`
/// is read whole before its three bytes are written at `3i`, which never passes `4i`.
fn flatten_onto_white(rgba: image::RgbaImage) -> Option<image::RgbImage> {
    let (width, height) = rgba.dimensions();
    let mut raw = rgba.into_raw();
    let pixels = raw.len() / 4;
    for i in 0..pixels {
        let [r, g, b, a] = [raw[4 * i], raw[4 * i + 1], raw[4 * i + 2], raw[4 * i + 3]];
        let a = u32::from(a);
        let over_white = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        raw[3 * i] = over_white(r);
        raw[3 * i + 1] = over_white(g);
        raw[3 * i + 2] = over_white(b);
    }
    raw.truncate(pixels * 3);
    image::RgbImage::from_raw(width, height, raw)
}

fn encode_jpeg(rgb: &image::RgbImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, TRANSCODE_JPEG_QUALITY)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(out)
}

fn encode_png(rgba: &image::RgbaImage) -> Option<Vec<u8>> {
    use image::ImageEncoder as _;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn webp_bytes(img: &image::DynamicImage) -> Vec<u8> {
        let mut out = Vec::new();
        img.write_with_encoder(image::codecs::webp::WebPEncoder::new_lossless(&mut out))
            .unwrap();
        out
    }

    fn attachment(bytes: &[u8], mime: &str) -> ImageAttachment {
        ImageAttachment {
            data: STANDARD.encode(bytes),
            mime_type: mime.to_string(),
        }
    }

    fn decoded(img: &ImageAttachment) -> Vec<u8> {
        STANDARD.decode(&img.data).unwrap()
    }

    fn opaque(width: u32, height: u32) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x * 7) as u8, (y * 5) as u8, 90])
        }))
    }

    fn translucent(width: u32, height: u32) -> image::DynamicImage {
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, _| {
            image::Rgba([200, 30, 30, if x % 2 == 0 { 0 } else { 255 }])
        }))
    }

    #[test]
    fn the_magic_numbers_identify_each_container() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Container::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(Container::Png));
        assert_eq!(sniff(b"GIF89a......"), Some(Container::Gif));
        assert_eq!(sniff(b"BM\x00\x00"), Some(Container::Bmp));
        assert_eq!(
            sniff(b"RIFF\x10\x00\x00\x00WEBPVP8L"),
            Some(Container::WebP)
        );
        // A RIFF that is not WebP (a WAV) is not a picture at all.
        assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WAVEfmt "), None);
        assert_eq!(sniff(b"%PDF-1.7"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn the_head_decode_skips_whitespace_and_a_data_url_prefix() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        let b64 = STANDARD.encode(png);
        let wrapped = format!("data:image/png;base64,{}\n{}", &b64[..6], &b64[6..]);
        let head = head_bytes(payload(&wrapped)).unwrap();
        assert_eq!(sniff(&head), Some(Container::Png));
        // Short payloads decode as they stand.
        assert_eq!(head_bytes("/9j/").unwrap(), vec![0xFF, 0xD8, 0xFF]);
        assert!(head_bytes("!!!! not base64").is_none());
    }

    #[tokio::test]
    async fn a_decodable_picture_passes_untouched() {
        let jpeg = {
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new(&mut out)
                .encode(&[10, 20, 30], 1, 1, image::ExtendedColorType::Rgb8)
                .unwrap();
            out
        };
        let input = vec![attachment(&jpeg, "image/jpeg")];
        let out = normalize_images_for_engine(input.clone()).await.unwrap();
        assert_eq!(out, input, "a JPEG must reach the engine byte-identical");
    }

    #[tokio::test]
    async fn a_mislabelled_picture_gets_the_label_of_its_bytes() {
        let png = {
            let mut out = Vec::new();
            opaque(2, 2)
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        let out = normalize_images_for_engine(vec![attachment(&png, "image/webp")])
            .await
            .unwrap();
        assert_eq!(out[0].mime_type, "image/png");
        assert_eq!(decoded(&out[0]), png, "a relabel never touches the bytes");
    }

    #[tokio::test]
    async fn an_opaque_webp_becomes_a_jpeg_even_when_labelled_jpeg() {
        let webp = webp_bytes(&opaque(40, 30));
        // Labelled image/jpeg on purpose: the label is the thing that cannot be trusted.
        let out = normalize_images_for_engine(vec![attachment(&webp, "image/jpeg")])
            .await
            .unwrap();
        assert_eq!(out[0].mime_type, "image/jpeg");
        let bytes = decoded(&out[0]);
        assert_eq!(sniff(&bytes), Some(Container::Jpeg));
        let back = image::load_from_memory(&bytes).unwrap();
        assert_eq!((back.width(), back.height()), (40, 30));
    }

    #[tokio::test]
    async fn a_transparent_webp_becomes_a_png_that_keeps_its_transparency() {
        let webp = webp_bytes(&translucent(16, 8));
        let out = normalize_images_for_engine(vec![attachment(&webp, "image/webp")])
            .await
            .unwrap();
        assert_eq!(out[0].mime_type, "image/png");
        let back = image::load_from_memory(&decoded(&out[0])).unwrap();
        assert!(back.color().has_alpha());
        assert_eq!(back.to_rgba8().get_pixel(0, 0).0[3], 0);
    }

    #[tokio::test]
    async fn an_rgba_webp_with_no_transparency_is_sent_as_jpeg() {
        let solid = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([1, 2, 3, 255]),
        ));
        let out = normalize_images_for_engine(vec![attachment(&webp_bytes(&solid), "image/webp")])
            .await
            .unwrap();
        assert_eq!(out[0].mime_type, "image/jpeg");
    }

    #[tokio::test]
    async fn garbage_behind_a_webp_header_is_unreadable_at_its_own_index() {
        let jpeg_ok = attachment(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0], "image/jpeg");
        let broken = attachment(
            b"RIFF\x20\x00\x00\x00WEBPVP8 this is not a picture",
            "image/webp",
        );
        let err = normalize_images_for_engine(vec![jpeg_ok, broken])
            .await
            .unwrap_err();
        assert_eq!(err, Unreadable { index: 1 });
    }

    #[tokio::test]
    async fn content_that_is_no_accepted_container_is_unreadable() {
        let pdf = attachment(b"%PDF-1.7 not a picture", "image/png");
        assert_eq!(
            normalize_images_for_engine(vec![pdf]).await.unwrap_err(),
            Unreadable { index: 0 }
        );
        let not_base64 = ImageAttachment {
            data: "!!!!!!!!!!!!!!!!".into(),
            mime_type: "image/jpeg".into(),
        };
        assert_eq!(
            normalize_images_for_engine(vec![not_base64])
                .await
                .unwrap_err(),
            Unreadable { index: 0 }
        );
    }

    #[tokio::test]
    async fn a_webp_past_the_edge_limit_is_refused_not_decoded() {
        let wide = webp_bytes(&opaque(MAX_DECODE_EDGE_PX + 1, 1));
        assert_eq!(
            normalize_images_for_engine(vec![attachment(&wide, "image/webp")])
                .await
                .unwrap_err(),
            Unreadable { index: 0 }
        );
    }

    #[test]
    fn flattening_composites_over_white_in_place() {
        let rgba = image::RgbaImage::from_raw(
            3,
            1,
            vec![
                0, 0, 0, 0, // fully transparent black -> white
                10, 20, 30, 255, // opaque -> unchanged
                0, 0, 0, 128, // half black -> mid grey
            ],
        )
        .unwrap();
        let rgb = flatten_onto_white(rgba).unwrap();
        assert_eq!(
            rgb.as_raw(),
            &vec![255, 255, 255, 10, 20, 30, 127, 127, 127]
        );
    }
}
