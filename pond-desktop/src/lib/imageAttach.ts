// Chat image attachments, downscaled and validated before anything is sent. The limits mirror
// pond-core's models/domain/image_limits.rs.

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

// The encoder resamples to a fixed patch grid, so beyond 1024px only costs decode memory (8 GB Jetson).
export const MAX_IMAGE_EDGE_PX = 1024;

export type SupportedImageMimeType = (typeof SUPPORTED_IMAGE_MIME_TYPES)[number];

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

/** Decoded length of padded or unpadded base64, without decoding; tolerates a `data:` prefix. */
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

/** The DOM decode step, isolated so the pure helpers stay testable without an image decoder. */
function decodeImageElement(objectUrl: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("Failed to decode image"));
    img.src = objectUrl;
  });
}

/** Draw `img` onto an offscreen canvas, downscaled so the longest edge is at most `maxEdge`. */
function downscaleToJpeg(
  img: HTMLImageElement,
  maxEdge: number,
  quality: number,
): { dataUrl: string; width: number; height: number } {
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
  // Canvas captures only the current frame, so an animated GIF flattens to a still; fine for one turn.
  ctx.drawImage(img, 0, 0, width, height);
  return { dataUrl: canvas.toDataURL("image/jpeg", quality), width, height };
}

/**
 * Validates, downscales if needed and base64-encodes an image; throws a readable Error on bad MIME.
 * An image within both limits keeps its original bytes, so small GIFs stay animated.
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

  if (withinEdgeLimit && withinByteLimit) {
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

  const { dataUrl, width, height } = downscaleToJpeg(img, MAX_IMAGE_EDGE_PX, 0.85);
  URL.revokeObjectURL(objectUrl);
  const { data } = parseDataUrl(dataUrl);
  return {
    data,
    mime_type: "image/jpeg",
    previewUrl: dataUrl,
    width,
    height,
    byteSize: decodedBase64Length(data),
  };
}

/** Checks pending plus newly picked images against the per-turn caps: an error message, or null. */
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
