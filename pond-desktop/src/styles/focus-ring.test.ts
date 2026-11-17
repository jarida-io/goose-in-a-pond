import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const BASE_CSS = join(HERE, "base.css");
const SRC = join(HERE, "..");

/** A non-inset, unblurred spread shadow: a solid ring drawn outside the box. */
const HALO = /box-shadow:(?![^;]*\binset\b)[^;]*\b0\s+0\s+0\s+\d+px/;

function cssFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return cssFiles(path);
    return name.endsWith(".css") ? [path] : [];
  });
}

/** Every rule whose selector is a focus state of the element itself (not a slider thumb). */
function focusRules(css: string): Array<{ selector: string; body: string }> {
  const plain = css.replace(/\/\*[\s\S]*?\*\//g, "");
  return [...plain.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
    .map(([, selector, body]) => ({ selector: selector.trim(), body }))
    .filter(({ selector }) => /:focus(-visible|-within)?\b/.test(selector) && !/::/.test(selector));
}

/** Selectors of the `!important` focus-visible rule, read from source: jsdom skips external CSS. */
function importantFocusSelectors(): string[] {
  // Strip comments first, or the rule's own comment gets glued to its selector list.
  const css = readFileSync(BASE_CSS, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  const match = css.match(
    /([^};]*)\{[^}]*outline:\s*2px solid var\(--focus-ring\) !important[^}]*\}/,
  );
  if (match === null) throw new Error("the !important focus-visible rule is gone from base.css");
  return match[1]
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

describe("the focus ring", () => {
  it("does not draw a second ring around a text field", () => {
    // Text fields already get an accent border plus halo on focus; this outline doubled it.
    const selectors = importantFocusSelectors();

    for (const element of ["input", "textarea", "select"]) {
      expect(
        selectors.some((s) => s.startsWith(`${element}:`)),
        `${element} is back in the !important focus-visible rule, which draws a ` +
          `second ring outside the border and halo it already has`,
      ).toBe(false);
    }
  });

  it("still guarantees a ring for everything that has no other indicator", () => {
    // Several component stylesheets set `outline: none` with no replacement; this is their net.
    const selectors = importantFocusSelectors();

    for (const element of ["button", "a", '[role="button"]']) {
      expect(selectors).toContain(`${element}:focus-visible`);
    }
    // Any other focusable element, text fields excepted: they have their own ring.
    expect(selectors).toContain("[tabindex]:not(input):not(textarea):not(select):focus-visible");
  });

  it("does not draw a second ring around a text field given a tabindex", () => {
    const selectors = importantFocusSelectors();

    expect(
      selectors.filter((s) => s.startsWith("[tabindex]")),
      "a bare [tabindex] selector reaches inputs and puts the !important outline back on them",
    ).toEqual(["[tabindex]:not(input):not(textarea):not(select):focus-visible"]);
  });

  it("leaves text fields a focus indicator of their own", () => {
    // `:focus`, not `:focus-visible`: a text field needs an indicator on any focus, even autoFocus.
    const css = readFileSync(BASE_CSS, "utf8");
    const rule = css.match(/input:focus,\s*textarea:focus,\s*select:focus\s*\{([^}]*)\}/);

    expect(rule, "text fields have no :focus rule at all").not.toBeNull();
    expect(rule?.[1]).toContain("border-color: var(--color-accent)");
    // A halo here also lands on the borderless field inside a wrapper that draws
    // its own focus border (Ink's `.ink-input__box`), ringing it twice.
    expect(rule?.[1]).not.toContain("box-shadow");
  });

  it("never pairs a focus border with a halo ring outside it", () => {
    const offenders = cssFiles(SRC).flatMap((path) =>
      focusRules(readFileSync(path, "utf8"))
        .filter(({ body }) => /\bborder(-color)?\s*:/.test(body) && HALO.test(body))
        .map(({ selector }) => `${relative(SRC, path)}: ${selector}`),
    );

    expect(offenders, "one ring per control: recolour the border, do not also add a halo").toEqual([]);
  });

  // A text field shows its focus state on every click, so a second ring there is seen by everyone;
  // a button's outline appears only under the keyboard, where it is the one place focus shows.
  it("never draws a focus outline outside a text field's own border", () => {
    const offenders = cssFiles(SRC).flatMap((path) => {
      const css = readFileSync(path, "utf8");
      const plain = css.replace(/\/\*[\s\S]*?\*\//g, "");
      // Elements this file gives a visible border of their own.
      const bordered = new Set(
        [...plain.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
          .filter(([, , body]) => /(^|[;\s])border\s*:\s*(?!none|0[;\s])/.test(body))
          .flatMap(([, selector]) => selector.split(",").map((part) => part.trim())),
      );
      return focusRules(css)
        .filter(({ body }) => {
          const outline = body.match(/(^|[;\s])outline\s*:\s*([^;]+);/);
          const offset = body.match(/outline-offset\s*:\s*([^;]+);/);
          return outline && !/\bnone\b/.test(outline[2]) && !(offset && offset[1].trim().startsWith("-"));
        })
        .flatMap(({ selector }) =>
          selector
            .split(",")
            .map((part) => part.trim())
            .filter((part) => /:focus/.test(part))
            .filter((part) => /\b(input|select|textarea)\b|__input|Input\b|-field|__select/.test(part))
            .filter((part) => bordered.has(part.replace(/:has\([^)]*:focus[^)]*\)/, "").replace(/:focus(-visible|-within)?/, "").trim()))
            .map((part) => `${relative(SRC, path)}: ${part}`),
        );
    });

    expect(
      offenders,
      "one ring per control: recolour the border, or draw the outline over it (negative offset)",
    ).toEqual([]);
  });
});
