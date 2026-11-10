// The goose's motion is pure: these pin the properties the eye depends on. Every outline is a closed,
// finite path; the head stays on the neck in every pose; the loops repeat; and listening, working and
// reduced motion each look the way the component promises.
import { describe, expect, it } from "vitest";
import {
  BANDS, BAND_PERIOD, GLINTS, WAVES, neckFrame, neckPose, neckWidth, partsFrame, restingParts, smoothPath, swell,
  type GooseMood,
} from "./gooseMotion";

const MOODS: GooseMood[] = ["idle", "working", "listening"];
const numbers = (d: string) => (d.match(/-?\d+(\.\d+)?/g) ?? []).map(Number);

describe("the neck and head", () => {
  it("are closed, finite outlines at every moment of a minute, in every mood", () => {
    for (const mood of MOODS) {
      for (let t = 0; t < 60; t += 0.37) {
        const f = neckFrame(neckPose(t, mood, mood === "listening" ? 1 : 0));
        for (const d of [f.neck, f.head]) {
          expect(d.startsWith("M")).toBe(true);
          expect(d.endsWith("Z")).toBe(true);
          expect(numbers(d).every(Number.isFinite)).toBe(true);
        }
        expect(f.headMatrix.every(Number.isFinite)).toBe(true);
      }
    }
  });

  it("keep the head on the neck: the throat sits where the spine ends, whatever the pose", () => {
    for (let t = 0; t < 34; t += 0.5) {
      const pose = neckPose(t, "idle", 0);
      const [, , , , e, f] = neckFrame(pose).headMatrix;
      expect(e).toBeCloseTo(pose.spine[3][0], 6);
      expect(f).toBeCloseTo(pose.spine[3][1], 6);
    }
  });

  it("is thickest at the chest, slimmest up the throat, and swells into the head", () => {
    expect(neckWidth(0)).toBeGreaterThan(neckWidth(0.5));
    expect(neckWidth(0.78)).toBeLessThan(neckWidth(1));
    expect(neckWidth(0.78)).toBeLessThan(neckWidth(0.5));
  });

  it("lowers its head to look into the water once every 17 seconds, and only when idle", () => {
    const lowest = (mood: GooseMood) => {
      let y = -Infinity;
      for (let t = 0; t < 17; t += 0.05) y = Math.max(y, neckPose(t, mood, 0).spine[3][1]);
      return y;
    };
    expect(lowest("idle")).toBeGreaterThan(29 + 6);
    expect(lowest("working")).toBeLessThan(29 + 2);
  });

  it("blinks", () => {
    const lids = Array.from({ length: 430 }, (_, i) => neckPose(i / 100, "idle", 0).lid);
    expect(Math.min(...lids)).toBeLessThan(0.2);
    expect(lids.filter((l) => l === 1).length).toBeGreaterThan(400);
  });

  it("straightens and tilts its head toward you while listening", () => {
    const at = (listen: number) => neckPose(2, "listening", listen);
    expect(at(1).spine[3][1]).toBeLessThan(at(0).spine[3][1]);
    expect(at(1).tilt).toBeLessThan(at(0).tilt);
  });
});

describe("the pond", () => {
  it("slides each wave within one wavelength and each band within one dash period, so the loops have no seam", () => {
    for (const mood of MOODS) {
      for (let t = 0; t < 40; t += 0.73) {
        const f = partsFrame(t, mood, 0);
        f.waves.forEach((x, i) => { expect(x).toBeLessThanOrEqual(0); expect(x).toBeGreaterThanOrEqual(-WAVES[i][2]); });
        f.bands.forEach((x) => { expect(x).toBeLessThanOrEqual(0); expect(x).toBeGreaterThanOrEqual(-BAND_PERIOD); });
      }
    }
    for (const [, , dash] of BANDS) expect(dash.split(" ").map(Number).reduce((s, n) => s + n, 0)).toBe(BAND_PERIOD);
  });

  it("opens a wake only while working, and rings only while listening", () => {
    expect(partsFrame(3, "idle", 0).wake.every((w) => w.opacity === 0)).toBe(true);
    expect(partsFrame(3, "working", 0).wake.every((w) => w.opacity > 0)).toBe(true);
    expect(partsFrame(3, "idle", 0).rings.every((r) => r.opacity === 0)).toBe(true);
    expect(partsFrame(3, "listening", 1).rings.some((r) => r.opacity > 0)).toBe(true);
  });

  it("goes still at rest: no ripples, the glints showing", () => {
    const rest = restingParts("idle");
    expect(rest.ripples.every((r) => r.opacity === 0)).toBe(true);
    expect(rest.glints).toHaveLength(GLINTS.length);
    expect(rest.glints.every((g) => g.opacity > 0)).toBe(true);
  });
});

describe("the helpers", () => {
  it("swell rises to 1 at mid-period and returns to 0", () => {
    expect(swell(0, 4)).toBeCloseTo(0);
    expect(swell(2, 4)).toBeCloseTo(1);
    expect(swell(4, 4)).toBeCloseTo(0);
  });

  it("smoothPath passes through every point", () => {
    const d = smoothPath([[0, 0], [10, 0], [10, 10]], false);
    expect(d).toMatch(/^M0\.00 0\.00/);
    expect(d).toContain("10.00 10.00");
  });
});
