import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const STYLES = dirname(fileURLToPath(import.meta.url));
const SRC = join(STYLES, "..");
const TOUCH_CSS = join(STYLES, "touch.css");

/** Selector lists of every rule in a stylesheet, comments dropped so none rides along. */
function selectorLists(css: string): string[] {
  return [...css.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/([^{}]+)\{[^{}]*\}/g)].map((m) =>
    m[1].trim(),
  );
}

/** Classes touch.css gives an invisible hit-area `::after` (`.reach` is opt-in markup, not a class list). */
function reachedClasses(): string[] {
  const found = new Set<string>();
  for (const list of selectorLists(readFileSync(TOUCH_CSS, "utf8"))) {
    for (const selector of list.split(",")) {
      const m = selector.trim().match(/^\.([\w-]+)::after$/);
      if (m && m[1] !== "reach") found.add(m[1]);
    }
  }
  return [...found];
}

function stylesheets(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return stylesheets(path);
    return entry.name.endsWith(".css") && path !== TOUCH_CSS ? [path] : [];
  });
}

describe("the touch layer's hit areas", () => {
  it("still reaches the dense controls", () => {
    // Guards the guard: an empty list would make the test below pass vacuously.
    expect(reachedClasses()).toEqual(expect.arrayContaining(["ext-tab-bar__tab", "ch-chip"]));
  });

  it("is the only ::after a reached control has", () => {
    // touch.css loads last, so its 44px centred box wins every equal-specificity
    // property of any other ::after on the same element. The Extensions tab
    // underline was one: it became a solid accent block over the selected label.
    const classes = reachedClasses();
    const offenders: string[] = [];

    for (const file of stylesheets(SRC)) {
      for (const list of selectorLists(readFileSync(file, "utf8"))) {
        for (const selector of list.split(",")) {
          for (const cls of classes) {
            // The class itself or a BEM modifier of it (`--active`), then ::after on that element.
            const onSameElement = new RegExp(`\\.${cls}(--[\\w-]+)?(?![\\w-])[^\\s>+~]*::?after`);
            if (onSameElement.test(selector)) {
              offenders.push(`${relative(SRC, file)}: ${selector.trim()}`);
            }
          }
        }
      }
    }

    expect(offenders, "draw these with ::before or a box-shadow instead").toEqual([]);
  });
});
