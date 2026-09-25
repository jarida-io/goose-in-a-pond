import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  ACCENTS,
  ACCENTS_DARK,
  createTheme,
  cssVariables,
  parseColor,
  ratio,
} from "@jarida/ink";
import { ACCENT_PALETTES } from "./hub/state/themeStore";

/**
 * Checks the app has not drifted from `@jarida/ink`, which lives outside this repo and cannot see
 * it. Also keeps the sibling-checkout `@jarida/ink` alias (vite/vitest configs) imported.
 */

const HERE = dirname(fileURLToPath(import.meta.url));
const TOKENS_CSS = resolve(HERE, "styles/design-tokens.css");

/** One property block; later declarations win like the cascade (`--bg-brand-soft` appears twice). */
function parseBlock(css: string, selector: string): Record<string, string> {
  const start = css.indexOf(selector);
  expect(start, `${selector} is missing from design-tokens.css`).toBeGreaterThan(-1);
  const end = css.indexOf("\n}", start);
  const body = css.slice(start, end);
  const out: Record<string, string> = {};
  for (const line of body.split("\n")) {
    const match = line.match(/^\s*(--[a-z0-9-]+)\s*:\s*(.+?);\s*(?:\/\*.*)?$/i);
    if (match) out[match[1]] = match[2].trim();
  }
  return out;
}

/** Resolves var() chains; `--pp*` come from the theme store at runtime, not the stylesheet's fallbacks. */
function resolver(vars: Record<string, string>, ramp: readonly string[]) {
  const runtime: Record<string, string> = {
    "--pp": ramp[0],
    "--pp-600": ramp[1],
    "--pp-100": ramp[2],
    "--pp-50": ramp[3],
  };

  function resolve(value: string, depth = 0): string {
    if (depth > 8) return value;
    let out = value;

    // color-mix(in srgb, <colour> <pct>%, transparent) — the only form used.
    const mix = out.match(/^color-mix\(in srgb,\s*(.+?)\s+(\d+)%,\s*transparent\)$/);
    if (mix) {
      const base = resolve(mix[1], depth + 1);
      const hex = base.replace("#", "");
      const rgb = [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16));
      return `rgba(${rgb[0]}, ${rgb[1]}, ${rgb[2]}, ${Number(mix[2]) / 100})`;
    }

    out = out.replace(/var\((--[a-z0-9-]+)(?:,\s*([^)]+))?\)/gi, (_all, name, fallback) => {
      const found = runtime[name] ?? vars[name];
      return resolve(found ?? fallback ?? "", depth + 1);
    });
    return out.trim();
  }

  return resolve;
}

const css = readFileSync(TOKENS_CSS, "utf8");

const light = parseBlock(css, ":root {");
const darkOverrides = parseBlock(css, ':root[data-theme="dark"] {');
// Dark only overrides. Anything it does not name is still whatever :root said.
const dark = { ...light, ...darkOverrides };

const resolveLight = resolver(light, ACCENTS.Purple);
const resolveDark = resolver(dark, ACCENTS_DARK.Purple);

/** Canonicalises anything that parses as a colour (the dark block pads its columns); rest is verbatim. */
function canonical(value: string): string {
  try {
    const { r, g, b, a } = parseColor(value);
    const round = (n: number) => Math.round(n);
    return a >= 1
      ? `#${[r, g, b].map((n) => round(n).toString(16).padStart(2, "0")).join("")}`
      : `rgba(${round(r)}, ${round(g)}, ${round(b)}, ${a})`;
  } catch {
    return value.toLowerCase();
  }
}

function drift(
  emitted: Record<string, string>,
  shipped: Record<string, string>,
  resolveValue: (value: string) => string,
  exempt: Record<string, string>,
): string[] {
  const mismatches: string[] = [];
  for (const [name, mine] of Object.entries(emitted)) {
    if (name in exempt) continue;
    const theirs = shipped[name];
    if (theirs === undefined) continue;
    const resolved = resolveValue(theirs);
    if (canonical(resolved) !== canonical(mine)) {
      mismatches.push(
        `${name}\n    shipped:  ${theirs}\n    resolved: ${resolved}\n    library:  ${mine}`,
      );
    }
  }
  return mismatches;
}

describe("light", () => {
  const emitted = cssVariables(createTheme());

  /** No exemptions: per DESIGN.md's precedence rule the shipped light theme wins. */
  it("reproduces the shipped stylesheet exactly", () => {
    const mismatches = drift(emitted, light, resolveLight, {});
    expect(
      mismatches,
      `The library and design-tokens.css have drifted:\n\n${mismatches.join("\n\n")}\n`,
    ).toEqual([]);
  });

  it("covers enough of it to be worth calling a source of truth", () => {
    const shared = Object.keys(emitted).filter((name) => name in light);
    // ~60 tokens is all a component reads; the rest (orb, memory-segment, role hues) is product-only.
    expect(shared.length).toBeGreaterThan(60);
  });
});

describe("dark", () => {
  const emitted = cssVariables(createTheme({ scheme: "dark" }));

  /** One exemption, a fix rather than a preference (asserted below). */
  const DIVERGENCES: Record<string, string> = {
    "--border-ink": "the plate has to survive the theme; see the test below",
    "--shadow-ink": "composed from --border-ink",
    "--color-accent-line": "derived; the shipped file has no such token in dark",
    "--focus-ring": "derived from the accent line",
    "--bg-brand-soft": "a solid paper, not a tint of the accent",
    "--pp-50": "same",
  };

  it("reproduces the shipped dark overrides", () => {
    const mismatches = drift(emitted, dark, resolveDark, DIVERGENCES);
    expect(
      mismatches,
      `The library and the dark block have drifted:\n\n${mismatches.join("\n\n")}\n`,
    ).toEqual([]);
  });

  it("diverges on the plate because the shipped one is invisible", () => {
    // Shipped dark mode keeps --border-ink #5D23C2 on #131119: 2.21:1 (2.00:1 on the panel).
    const shippedPlate = resolveDark(dark["--border-ink"]);
    expect(shippedPlate.toLowerCase()).toBe("#5d23c2");
    expect(ratio(shippedPlate, "#131119")).toBeLessThan(3);
    expect(ratio(shippedPlate, "#1E1B26")).toBeLessThan(3);
    expect(ratio(createTheme({ scheme: "dark" }).color.ink, "#131119")).toBeGreaterThanOrEqual(3);
  });

  it("keeps no exemption that has stopped being one", () => {
    // A stale exemption would hide a real drift later.
    for (const name of Object.keys(DIVERGENCES)) {
      const theirs = dark[name];
      if (theirs === undefined) continue;
      expect(
        canonical(resolveDark(theirs)),
        `${name} now matches — remove it from DIVERGENCES`,
      ).not.toBe(canonical(emitted[name]));
    }
  });
});

describe("the accent chain", () => {
  it("uses the palette the theme store actually writes at boot", () => {
    // The stylesheet falls back to #8C4BFF, but the store writes #7C3AED at boot.
    for (const [name, ramp] of Object.entries(ACCENTS)) {
      expect(ACCENT_PALETTES[name as keyof typeof ACCENT_PALETTES], `${name} has drifted`).toEqual([
        ...ramp,
      ]);
    }
  });

  it("resolves the library through the alias", () => {
    expect(typeof createTheme).toBe("function");
  });
});
