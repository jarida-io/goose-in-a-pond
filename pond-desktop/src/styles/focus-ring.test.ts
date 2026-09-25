import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const BASE_CSS = join(dirname(fileURLToPath(import.meta.url)), "base.css");

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

    for (const element of ["button", "a", '[role="button"]', "[tabindex]"]) {
      expect(selectors).toContain(`${element}:focus-visible`);
    }
  });

  it("leaves text fields a focus indicator of their own", () => {
    // `:focus`, not `:focus-visible`: a text field needs an indicator on any focus, even autoFocus.
    const css = readFileSync(BASE_CSS, "utf8");
    const rule = css.match(/input:focus,\s*textarea:focus,\s*select:focus\s*\{([^}]*)\}/);

    expect(rule, "text fields have no :focus rule at all").not.toBeNull();
    expect(rule?.[1]).toContain("border-color: var(--color-accent)");
    expect(rule?.[1]).toContain("box-shadow: 0 0 0 3px var(--color-accent-subtle)");
  });
});
