import { describe, it, expect } from "vitest";
import {
  parseBounds,
  isOnSomeDisplay,
  usableBounds,
  stateFilePath,
} from "./windowState";

const LAPTOP = { x: 0, y: 0, width: 1512, height: 982 };
/** A second display to the right — the kind that gets unplugged. */
const EXTERNAL = { x: 1512, y: 0, width: 2560, height: 1440 };

describe("parseBounds", () => {
  it("reads a well-formed state file", () => {
    expect(parseBounds('{"x":10,"y":20,"width":1280,"height":860}')).toEqual({
      x: 10,
      y: 20,
      width: 1280,
      height: 860,
    });
  });

  it("refuses anything it cannot trust", () => {
    expect(parseBounds(null)).toBe(null);
    expect(parseBounds("")).toBe(null);
    expect(parseBounds("not json")).toBe(null);
    expect(parseBounds("[1,2,3]")).toBe(null);
    expect(parseBounds('{"x":0,"y":0,"width":1280}')).toBe(null);
    expect(parseBounds('{"x":"0","y":0,"width":1280,"height":860}')).toBe(null);
    expect(parseBounds('{"x":0,"y":0,"width":null,"height":860}')).toBe(null);
  });

  // A file truncated mid-write, or hand-edited, must not produce a window with
  // no area — which is a window you can neither see nor grab.
  it("treats a zero or negative size as no state", () => {
    expect(parseBounds('{"x":0,"y":0,"width":0,"height":860}')).toBe(null);
    expect(parseBounds('{"x":0,"y":0,"width":1280,"height":-5}')).toBe(null);
  });
});

describe("isOnSomeDisplay", () => {
  it("accepts a window fully on a display", () => {
    expect(
      isOnSomeDisplay({ x: 100, y: 100, width: 800, height: 600 }, [LAPTOP]),
    ).toBe(true);
  });

  // Overlap, not containment: a window hanging off an edge is a position the
  // user chose and can still drag back.
  it("accepts a window hanging off an edge", () => {
    expect(
      isOnSomeDisplay({ x: -200, y: 50, width: 800, height: 600 }, [LAPTOP]),
    ).toBe(true);
    expect(
      isOnSomeDisplay({ x: 1400, y: 900, width: 800, height: 600 }, [LAPTOP]),
    ).toBe(true);
  });

  it("accepts a window on a second display while it is attached", () => {
    expect(
      isOnSomeDisplay({ x: 2000, y: 300, width: 800, height: 600 }, [
        LAPTOP,
        EXTERNAL,
      ]),
    ).toBe(true);
  });

  // The failure this whole module exists for.
  it("rejects a window on a display that has been unplugged", () => {
    expect(
      isOnSomeDisplay({ x: 2000, y: 300, width: 800, height: 600 }, [LAPTOP]),
    ).toBe(false);
  });

  it("rejects a window that touches nothing at all", () => {
    expect(
      isOnSomeDisplay({ x: 9000, y: 9000, width: 800, height: 600 }, [
        LAPTOP,
        EXTERNAL,
      ]),
    ).toBe(false);
    expect(isOnSomeDisplay({ x: 0, y: 0, width: 800, height: 600 }, [])).toBe(
      false,
    );
  });

  // Abutting is not overlapping: a window exactly beside a display has no
  // pixel on it.
  it("rejects a window that only abuts a display", () => {
    expect(
      isOnSomeDisplay({ x: 1512, y: 0, width: 800, height: 600 }, [LAPTOP]),
    ).toBe(false);
  });
});

describe("usableBounds", () => {
  const saved = JSON.stringify({ x: 2000, y: 300, width: 800, height: 600 });

  it("restores a remembered position that is still reachable", () => {
    expect(usableBounds(saved, [LAPTOP, EXTERNAL])).toEqual({
      x: 2000,
      y: 300,
      width: 800,
      height: 600,
    });
  });

  it("falls back to the default when that display is gone", () => {
    expect(usableBounds(saved, [LAPTOP])).toBe(null);
  });

  it("falls back on a missing or corrupt file", () => {
    expect(usableBounds(null, [LAPTOP])).toBe(null);
    expect(usableBounds("{", [LAPTOP])).toBe(null);
  });
});

describe("stateFilePath", () => {
  it("sits in the app's own data directory", () => {
    expect(stateFilePath("/tmp/userData")).toBe(
      "/tmp/userData/window-state.json",
    );
  });
});
