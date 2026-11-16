import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..");

function files(dir: string, ext: RegExp): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return files(path, ext);
    return ext.test(name) && !/\.test\.tsx?$/.test(name) ? [path] : [];
  });
}

const stripComments = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, "");

describe("controls", () => {
  it("has one switch: nothing imports HeroUI's", () => {
    // HeroUI's switch is blue and its thumb a clipped pill; components/Switch.tsx is the app's.
    const offenders = files(SRC, /\.tsx?$/).filter((path) =>
      /import\s*\{[^}]*\bSwitch\b[^}]*\}\s*from\s*["']@heroui\/react["']/.test(readFileSync(path, "utf8")),
    );
    expect(offenders.map((p) => relative(SRC, p))).toEqual([]);
  });

  it("colours every offset shadow with the ink edge, not the accent", () => {
    // The accent offset reads as a second, coloured border; --border-ink is derived to stay 3:1.
    const accentOffset = /box-shadow:[^;]*\b\d+px\s+\d+px\s+0\s+var\(--(pp|color-accent)\)/;
    const offenders = files(SRC, /\.css$/).filter((path) =>
      accentOffset.test(stripComments(readFileSync(path, "utf8"))),
    );
    expect(offenders.map((p) => relative(SRC, p))).toEqual([]);
  });

  it("draws focus over a bordered tile's border, not a second ring around it", () => {
    const rules: Array<[string, RegExp]> = [
      ["styles/context.css", /\.ctx__view:focus-visible\s*\{[^}]*outline-offset:\s*-/],
      ["styles/context.css", /\.ctx__search:focus-within\s*\{[^}]*outline-offset:\s*-/],
      ["styles/sections.css", /\.secret-modal__choice-option:has\(input:focus-visible\)\s*\{[^}]*outline-offset:\s*-/],
      ["styles/settings-catalogue.css", /\.scat__radio:has\(input:focus-visible\)\s*\{[^}]*outline-offset:\s*-/],
    ];
    for (const [file, rule] of rules) {
      expect(rule.test(stripComments(readFileSync(join(SRC, file), "utf8"))), `${file}: ${rule}`).toBe(true);
    }
  });
  it("gives the shell bar's round buttons one edge, not a border plus an offset ring", () => {
    // The offset plate behind a bordered circle reads as a second ring around it.
    const css = stripComments(readFileSync(join(SRC, "hub/hubDrawer.css"), "utf8"));
    const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].filter(([, selector]) =>
      /\.shellbar__btn\b/.test(selector),
    );
    expect(rules.length).toBeGreaterThan(0);

    const offsetRing = /box-shadow:(?![^;]*\binset\b)[^;]*\d+px\s+\d+px\s+0\b|box-shadow:[^;]*var\(--ink-lift\)/;
    for (const [, selector, body] of rules) {
      expect(offsetRing.test(body), `${selector.trim()} draws an offset ring`).toBe(false);
    }

    // Keyboard focus keeps one indicator, drawn over the border rather than outside it.
    expect(css).toMatch(/\.shellbar__btn:focus-visible\s*\{[^}]*outline-offset:\s*-/);
  });
});
