// Client-side preparation of image attachments for the chat vision pipeline
// (Phase F1). Downscaling and MIME/size validation happen here, BEFORE
// anything reaches the network, so an oversized or unsupported image never
// costs a round trip to find out. Mirrors the server-side limits in
// pond-core (models/domain/image_limits.rs).

export const MAX_IMAGES_PER_TURN = 4;
export const MAX_IMAGE_BYTES = 4 * 1024 * 1024; // 4 MiB, decoded, per image
export const MAX_TOTAL_IMAGE_BYTES = 8 * 1024 * 1024; // 8 MiB, decoded, per turn
export const SUPPORTED_IMAGE_MIME_TYPES = [
  "image/jpeg",
  "image/png",
  "image/webp",
  "image/gif",
  "image/bmp",
] as const;

// A 2B-class vision encoder (the on-device Jetson target) gains essentially
// nothing above ~1024px on the longest edge — the encoder resamples to a
// fixed patch grid regardless — while a modern phone photo (12 MP+) decoded
// at full resolution would blow well past the 8GB Jetson's memory budget
// just to hold the RGB buffer. Downscaling client-side keeps the wire
// payload small AND keeps the server-side decode cheap.
export const MAX_IMAGE_EDGE_PX = 1024;

export type SupportedImageMimeType = (typeof SUPPORTED_IMAGE_MIME_TYPES)[number];

/**
 * MIME types the on-device vision encoder can actually decode — it uses
 * stb_image, which has no WebP support at all (mirrors pond-core's
 * `ENGINE_DECODABLE_IMAGE_TYPES`). WebP is still accepted at attach time (see
 * `SUPPORTED_IMAGE_MIME_TYPES`), but it is always re-encoded below, whatever
 * its size — the "keep the original bytes" fast path is for formats the
 * engine can actually read.
 */
export const ENGINE_DECODABLE_MIME = [
  "image/jpeg",
  "image/png",
  "image/gif",
  "image/bmp",
] as const;

/**
 * Which format a re-encode should target, given whether the decoded pixels
 * carry transparency. Pure, so it is unit-testable without a canvas — `mime`
 * is carried for a caller that wants to log or branch on the source format,
 * but the target format itself depends only on alpha: JPEG has no alpha
 * channel, so anything with one becomes PNG.
 */
export function reencodeTarget(
  _mime: string,
  hasAlpha: boolean,
): "image/png" | "image/jpeg" {
  return hasAlpha ? "image/png" : "image/jpeg";
}

export interface PreparedImage {
  /** Raw base64 (no `data:...;base64,` prefix) — matches the wire ImageAttachment shape. */
  data: string;
  mime_type: string;
  /** Object/data URL suitable for an <img src>. Caller owns revocation. */
  previewUrl: string;
  width: number;
  height: number;
  /** Decoded byte size of `data`. */
  byteSize: number;
}

/**
 * Compute the decoded byte length of a base64 string WITHOUT decoding it —
 * used to size-check an image before doing any real decode work. Handles
 * both padded ("...==" / "...=") and unpadded base64, and tolerates a
 * `data:...;base64,` prefix even though callers are expected to pass raw
 * base64.
 */
export function decodedBase64Length(b64: string): number {
  const commaIdx = b64.indexOf(",");
  const raw = b64.startsWith("data:") && commaIdx !== -1 ? b64.slice(commaIdx + 1) : b64;
  const len = raw.length;
  if (len === 0) return 0;
  if (raw.endsWith("==")) return (len / 4) * 3 - 2;
  if (raw.endsWith("=")) return (len / 4) * 3 - 1;
  return Math.floor((len * 3) / 4);
}

function isSupportedMime(mime: string): mime is SupportedImageMimeType {
  return (SUPPORTED_IMAGE_MIME_TYPES as readonly string[]).includes(mime);
}

/** Split a `data:<mime>;base64,<data>` URL into its parts. */
function parseDataUrl(dataUrl: string): { mime: string; data: string } {
  const match = /^data:([^;]+);base64,(.*)$/s.exec(dataUrl);
  if (!match) throw new Error("Failed to read image data");
  return { mime: match[1], data: match[2] };
}

function readAsDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read file"));
    reader.readAsDataURL(blob);
  });
}

/**
 * DOM-dependent decode step, isolated so the pure helpers above (and
 * validateAttachmentSet below) stay unit-testable in jsdom/happy-dom without
 * a real image decoder.
 */
function decodeImageElement(objectUrl: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("Failed to decode image"));
    img.src = objectUrl;
  });
}

/** Whether any sampled pixel is not fully opaque. Called on a canvas already
 *  downscaled to at most `MAX_IMAGE_EDGE_PX` per edge, so this is always a
 *  read of at most 1024x1024 pixels. */
function canvasHasAlpha(ctx: CanvasRenderingContext2D, width: number, height: number): boolean {
  const { data } = ctx.getImageData(0, 0, width, height);
  for (let i = 3; i < data.length; i += 4) {
    if (data[i] !== 255) return true;
  }
  return false;
}

/**
 * Draw `img` onto an offscreen canvas, downscaled so the longest edge is at
 * most `maxEdge`, and encode it as PNG (alpha present) or JPEG (opaque), per
 * `reencodeTarget`. A PNG that comes out over `MAX_IMAGE_BYTES` falls back to
 * JPEG — better a photo with its alpha flattened than one the size cap
 * refuses outright.
 *
 * JPEG has no alpha channel, so a transparent source needs a white background
 * filled BEFORE the draw: an unfilled canvas defaults to transparent BLACK,
 * which is what made a large transparent PNG render with a black backing.
 */
function downscaleAndEncode(
  img: HTMLImageElement,
  mime: string,
  maxEdge: number,
  quality: number,
): { dataUrl: string; width: number; height: number; mime: "image/png" | "image/jpeg" } {
  const w = img.naturalWidth;
  const h = img.naturalHeight;
  const scale = Math.min(1, maxEdge / Math.max(w, h));
  const width = Math.max(1, Math.round(w * scale));
  const height = Math.max(1, Math.round(h * scale));

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas 2D context unavailable");
  // Animated GIFs: an <img>/canvas draw only ever captures the CURRENTLY
  // decoded frame (the first, at load time), so re-encoding here
  // intentionally flattens an animated GIF to a single still frame — there
  // is no meaningful "motion" to preserve within one vision-model turn.
  ctx.drawImage(img, 0, 0, width, height);

  const target = reencodeTarget(mime, canvasHasAlpha(ctx, width, height));
  if (target === "image/png") {
    const dataUrl = canvas.toDataURL("image/png");
    if (decodedBase64Length(parseDataUrl(dataUrl).data) <= MAX_IMAGE_BYTES) {
      return { dataUrl, width, height, mime: "image/png" };
    }
    // Too large as PNG — fall through to the JPEG branch below.
  }

  // Re-drawn on a FRESH canvas rather than filling the one above: filling
  // white after drawing would paint over the image's own opaque pixels too.
  const jpegCanvas = document.createElement("canvas");
  jpegCanvas.width = width;
  jpegCanvas.height = height;
  const jctx = jpegCanvas.getContext("2d");
  if (!jctx) throw new Error("Canvas 2D context unavailable");
  jctx.fillStyle = "#fff";
  jctx.fillRect(0, 0, width, height);
  jctx.drawImage(img, 0, 0, width, height);
  return { dataUrl: jpegCanvas.toDataURL("image/jpeg", quality), width, height, mime: "image/jpeg" };
}

/**
 * Validate, downscale (if needed) and base64-encode a file/blob for the chat
 * vision pipeline. Throws a human-readable Error for unsupported MIME types.
 *
 * If the image is already within both the edge and byte limits, the
 * original bytes and MIME type are kept as-is (this is what preserves GIF
 * animation for small GIFs). Otherwise it's downscaled to at most
 * MAX_IMAGE_EDGE_PX on the longest edge and re-encoded as JPEG.
 */
export async function prepareImage(file: File | Blob): Promise<PreparedImage> {
  const mime = file.type;
  if (!isSupportedMime(mime)) {
    throw new Error(
      `Unsupported image type${mime ? ` "${mime}"` : ""}. Use JPEG, PNG, WebP, GIF, or BMP.`,
    );
  }

  const objectUrl = URL.createObjectURL(file);
  let img: HTMLImageElement;
  try {
    img = await decodeImageElement(objectUrl);
  } catch (e) {
    URL.revokeObjectURL(objectUrl);
    throw e;
  }

  const longestEdge = Math.max(img.naturalWidth, img.naturalHeight);
  const withinEdgeLimit = longestEdge <= MAX_IMAGE_EDGE_PX;
  const withinByteLimit = file.size <= MAX_IMAGE_BYTES;
  // The engine cannot read WebP at all (see ENGINE_DECODABLE_MIME) — keeping
  // the original bytes is only safe for a format it can actually decode, so a
  // small WebP still goes through the re-encode path below.
  const isEngineDecodable = (ENGINE_DECODABLE_MIME as readonly string[]).includes(mime);

  if (withinEdgeLimit && withinByteLimit && isEngineDecodable) {
    const dataUrl = await readAsDataUrl(file);
    const { data } = parseDataUrl(dataUrl);
    return {
      data,
      mime_type: mime,
      previewUrl: objectUrl,
      width: img.naturalWidth,
      height: img.naturalHeight,
      byteSize: decodedBase64Length(data),
    };
  }

  const { dataUrl, width, height, mime: outMime } = downscaleAndEncode(
    img,
    mime,
    MAX_IMAGE_EDGE_PX,
    0.85,
  );
  URL.revokeObjectURL(objectUrl);
  const { data } = parseDataUrl(dataUrl);
  return {
    data,
    mime_type: outMime,
    previewUrl: dataUrl,
    width,
    height,
    byteSize: decodedBase64Length(data),
  };
}

/**
 * Check a prospective attachment set (already-pending + newly picked)
 * against the per-turn caps. Returns a human-readable error message, or null
 * when the set is within limits.
 */
export function validateAttachmentSet(
  existing: PreparedImage[],
  incoming: PreparedImage[],
): string | null {
  const totalCount = existing.length + incoming.length;
  if (totalCount > MAX_IMAGES_PER_TURN) {
    return `You can attach up to ${MAX_IMAGES_PER_TURN} images per message.`;
  }
  const totalBytes = [...existing, ...incoming].reduce((sum, img) => sum + img.byteSize, 0);
  if (totalBytes > MAX_TOTAL_IMAGE_BYTES) {
    const totalMb = (totalBytes / (1024 * 1024)).toFixed(1);
    const capMb = (MAX_TOTAL_IMAGE_BYTES / (1024 * 1024)).toFixed(0);
    return `Attached images total ${totalMb} MB, which is over the ${capMb} MB limit per message.`;
  }
  return null;
}
