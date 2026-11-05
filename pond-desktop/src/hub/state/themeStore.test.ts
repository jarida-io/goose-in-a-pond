import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { act, renderHook } from "@testing-library/react";
import { afterAll, describe, expect, it, vi } from "vitest";
import { ACCENT_NAMES, ACCENTS, ratio } from "@jarida/ink";
import { SHIPPED_BG, useTheme } from "./themeStore";

const css = readFileSync(
  resolve(dirname(fileURLToPath(import.meta.url)), "../../styles/design-tokens.css"),
  "utf8",
);

function stylesheetToken(selector: string, name: string): string {
  const start = css.indexOf(selector);
  const block = css.slice(start, css.indexOf("\n}", start));
  const match = block.match(new RegExp(`^\\s*${name}\\s*:\\s*([^;]+);`, "m"));
  if (start < 0 || !match) throw new Error(`${name} is missing from ${selector} in design-tokens.css`);
  return match[1].trim();
}

const SCHEMES = {
  light: { selector: ":root {", choice: "Light" },
  dark: { selector: ':root[data-theme="dark"] {', choice: "Dark" },
} as const;

const GROUND_TOKENS = ["--color-bg", "--color-surface", "--color-content", "--color-sidebar"];

const root = document.documentElement;
const { setTheme, setAccent } = renderHook(() => useTheme()).result.current;
const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
afterAll(() => warn.mockRestore());

/** Inline beats the stylesheet, so a ground is ink's value where themeStore wrote one. */
function shippedGround(selector: string, name: string): string {
  return root.style.getPropertyValue(name) || stylesheetToken(selector, name);
}

describe("themeStore's ink edge", () => {
  for (const [scheme, { selector, choice }] of Object.entries(SCHEMES)) {
    for (const accent of ACCENT_NAMES) {
      it(`clears 3:1 on every ${scheme} ground with ${accent}`, () => {
        act(() => setTheme(choice));
        warn.mockClear();
        act(() => setAccent(accent));

        const edge = root.style.getPropertyValue("--border-ink");
        for (const name of GROUND_TOKENS) {
          const ground = shippedGround(selector, name);
          expect(ratio(edge, ground), `${edge} on ${name} ${ground}`).toBeGreaterThanOrEqual(3);
        }
        expect(root.style.getPropertyValue("--shadow-ink")).toContain(edge);
        expect(warn).toHaveBeenCalledTimes(edge === ACCENTS[accent][0] ? 0 : 1);
      });
    }
  }

  it("leaves Purple in light as the accent", () => {
    act(() => setTheme("Light"));
    act(() => setAccent("Purple"));
    expect(root.style.getPropertyValue("--border-ink")).toBe("#7C3AED");
  });

  it("measures the page the stylesheet ships, not ink's", () => {
    for (const [scheme, { selector, choice }] of Object.entries(SCHEMES)) {
      act(() => setTheme(choice));
      expect(root.style.getPropertyValue("--color-bg")).toBe("");
      expect(SHIPPED_BG[scheme as keyof typeof SHIPPED_BG].toLowerCase()).toBe(
        stylesheetToken(selector, "--color-bg").toLowerCase(),
      );
    }
  });
});
