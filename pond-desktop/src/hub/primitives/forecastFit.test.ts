import { describe, expect, it } from "vitest";
import { forecastFit } from "./forecastFit";

// A day tile is about 66px tall; 6px gaps.
const TILE_H = 66;

describe("forecastFit", () => {
  it("shows at least five days in one row on a medium card", () => {
    // A medium frame on its 300px basis leaves the strip about 258px wide.
    expect(forecastFit(7, 258, TILE_H, TILE_H)).toEqual({ shown: 5, columns: 5 });
  });

  it("shows the whole week in one row when the card is wide enough", () => {
    expect(forecastFit(7, 520, TILE_H, TILE_H)).toEqual({ shown: 7, columns: 7 });
  });

  it("uses a taller card for more days instead of empty space", () => {
    const oneRow = forecastFit(7, 258, TILE_H, TILE_H);
    const twoRows = forecastFit(7, 258, TILE_H * 2 + 6, TILE_H);

    expect(oneRow.shown).toBe(5);
    expect(twoRows).toEqual({ shown: 7, columns: 4 });
  });

  it("never shows more days than the forecast has", () => {
    expect(forecastFit(3, 600, 400, TILE_H)).toEqual({ shown: 3, columns: 3 });
  });

  it("keeps one row however short the strip is", () => {
    expect(forecastFit(7, 258, 10, TILE_H).shown).toBe(5);
  });

  it("shows nothing for an empty forecast", () => {
    expect(forecastFit(0, 600, 400, TILE_H)).toEqual({ shown: 0, columns: 1 });
  });
});
