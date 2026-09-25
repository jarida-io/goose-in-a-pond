import { describe, it, expect } from "vitest";
import {
  decodedBase64Length,
  validateAttachmentSet,
  MAX_IMAGES_PER_TURN,
  MAX_TOTAL_IMAGE_BYTES,
} from "./imageAttach";
import type { PreparedImage } from "./imageAttach";

function fakeImage(byteSize: number): PreparedImage {
  return {
    data: "",
    mime_type: "image/jpeg",
    previewUrl: "blob:fake",
    width: 100,
    height: 100,
    byteSize,
  };
}

describe("decodedBase64Length", () => {
  it("computes length for a full block with no padding", () => {
    expect(decodedBase64Length("AAAA")).toBe(3);
  });

  it("computes length with one padding char", () => {
    expect(decodedBase64Length("AAA=")).toBe(2);
  });

  it("computes length with two padding chars", () => {
    expect(decodedBase64Length("AA==")).toBe(1);
  });

  it("computes length for unpadded base64", () => {
    expect(decodedBase64Length("AAA")).toBe(2);
    expect(decodedBase64Length("AA")).toBe(1);
  });

  it("returns 0 for an empty string", () => {
    expect(decodedBase64Length("")).toBe(0);
  });

  it("matches a known real-world example", () => {
    // btoa("hello world") === "aGVsbG8gd29ybGQ=" — 11 decoded bytes.
    expect(decodedBase64Length("aGVsbG8gd29ybGQ=")).toBe(11);
  });

  it("strips a data: URL prefix defensively", () => {
    expect(decodedBase64Length("data:image/png;base64,aGVsbG8gd29ybGQ=")).toBe(11);
  });
});

describe("validateAttachmentSet", () => {
  it("allows a set at exactly the per-turn image count", () => {
    const existing = [fakeImage(100), fakeImage(100), fakeImage(100)];
    const incoming = [fakeImage(100)];
    expect(existing.length + incoming.length).toBe(MAX_IMAGES_PER_TURN);
    expect(validateAttachmentSet(existing, incoming)).toBeNull();
  });

  it("rejects a set exceeding the per-turn image count by one", () => {
    const existing = [fakeImage(100), fakeImage(100), fakeImage(100), fakeImage(100)];
    const incoming = [fakeImage(100)];
    const err = validateAttachmentSet(existing, incoming);
    expect(err).not.toBeNull();
    expect(err).toContain(String(MAX_IMAGES_PER_TURN));
  });

  it("allows a set at exactly the total byte cap", () => {
    const existing = [fakeImage(MAX_TOTAL_IMAGE_BYTES)];
    expect(validateAttachmentSet(existing, [])).toBeNull();
  });

  it("rejects a set exceeding the total byte cap by one byte", () => {
    const existing = [fakeImage(MAX_TOTAL_IMAGE_BYTES)];
    const incoming = [fakeImage(1)];
    expect(validateAttachmentSet(existing, incoming)).not.toBeNull();
  });

  it("returns null for an empty set", () => {
    expect(validateAttachmentSet([], [])).toBeNull();
  });
});
