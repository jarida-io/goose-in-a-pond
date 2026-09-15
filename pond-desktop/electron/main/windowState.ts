// Remembering where the window was.
//
// The Tauri shell got this from `tauri-plugin-window-state` with
// `StateFlags::POSITION | SIZE`, so it is behaviour users already have and
// would notice losing.
//
// The interesting part is not saving; it is refusing to restore. Move the
// window to a second display, quit, unplug that display, and a naive restore
// puts it at coordinates no screen covers -- off in the void, with no way to
// drag it back. So a remembered position is only honoured if some display
// still overlaps it.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Display {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Guard against a truncated or hand-edited file becoming a bad window. */
export function parseBounds(raw: string | null): Bounds | null {
  if (raw === null) return null;
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const b = value as Record<string, unknown>;
  const nums = ["x", "y", "width", "height"].map((k) => b[k]);
  if (!nums.every((n) => typeof n === "number" && Number.isFinite(n)))
    return null;
  const [x, y, width, height] = nums as number[];
  // A zero or negative size is not a window; treat it as no state at all.
  if (width! <= 0 || height! <= 0) return null;
  return { x: x!, y: y!, width: width!, height: height! };
}

/**
 * Does any display still overlap these bounds?
 *
 * Overlap rather than containment, deliberately: a window half off the edge of
 * a screen is a position the user chose and can still reach.
 */
export function isOnSomeDisplay(
  bounds: Bounds,
  displays: readonly Display[],
): boolean {
  return displays.some(
    (d) =>
      bounds.x + bounds.width > d.x &&
      bounds.x < d.x + d.width &&
      bounds.y + bounds.height > d.y &&
      bounds.y < d.y + d.height,
  );
}

/**
 * The bounds to actually open with: the remembered ones when they are still
 * reachable, otherwise null so the caller falls back to its default geometry.
 */
export function usableBounds(
  raw: string | null,
  displays: readonly Display[],
): Bounds | null {
  const saved = parseBounds(raw);
  if (saved === null) return null;
  return isOnSomeDisplay(saved, displays) ? saved : null;
}

export function stateFilePath(userDataDir: string): string {
  return join(userDataDir, "window-state.json");
}

export function readState(path: string): string | null {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return null;
  }
}

/** Best effort: failing to remember a window position is not worth an error. */
export function writeState(path: string, bounds: Bounds): void {
  try {
    writeFileSync(path, JSON.stringify(bounds), "utf8");
  } catch {
    // Nothing to do; the window just opens at its default next time.
  }
}
