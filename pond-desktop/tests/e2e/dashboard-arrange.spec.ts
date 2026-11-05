import { test, expect, type Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { contrastOf } from "./helpers/contrast";
import { navigateTo } from "./helpers/nav";

/**
 * Arrange mode and the Extensions tab bar, measured in a real layout engine: jsdom has none, so
 * overlap, containment and contrast can only be asserted here.
 */

type CardId = "weather" | "devices" | "nowPlaying" | "tonight" | "remoteAccess";

async function storeLayout(page: Page, pages: { id: CardId; size: "s" | "m" | "l" }[][]): Promise<void> {
  await page.addInitScript((layout) => {
    localStorage.setItem("giap-dashboard-layout", layout);
  }, JSON.stringify({ version: 2, pages, hidden: [] }));
}

async function startArranging(page: Page): Promise<void> {
  await page.goto("/");
  await page.getByRole("button", { name: "Arrange", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Arrange Home" })).toBeVisible();
}

/** Every pair of frames that intersect, and every frame whose toolbar reaches into its card. */
async function overlaps(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const found: string[] = [];
    const frames = [...document.querySelectorAll<HTMLElement>(".wtrack__page .wframe")];
    const label = (f: HTMLElement) => f.querySelector(".wframe__btn")?.getAttribute("aria-label") ?? "?";
    const rects = frames.map((f) => f.getBoundingClientRect());
    for (let i = 0; i < frames.length; i += 1) {
      for (let j = i + 1; j < frames.length; j += 1) {
        const [a, b] = [rects[i], rects[j]];
        if (a.left < b.right - 1 && b.left < a.right - 1 && a.top < b.bottom - 1 && b.top < a.bottom - 1) {
          found.push(`${label(frames[i])} intersects ${label(frames[j])}`);
        }
      }
    }
    for (const frame of frames) {
      const bar = frame.querySelector(".wframe__bar")?.getBoundingClientRect();
      const body = frame.querySelector(".wframe__body")?.getBoundingClientRect();
      if (bar && body && bar.bottom > body.top + 1) {
        found.push(`${label(frame)}: toolbar covers ${Math.round(bar.bottom - body.top)}px of its card`);
      }
    }
    return found;
  });
}

test.describe("Home while arranging", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  /**
   * 1150px is where two medium frames share a row with a small one and are squeezed to about
   * 300px, narrower than the toolbar's one-row width: the toolbar wrapped and its remove
   * button sat on the card below it.
   */
  test("a resize never puts one widget's controls on another widget", async ({ page }) => {
    await page.setViewportSize({ width: 1150, height: 800 });
    await storeLayout(page, [
      [
        { id: "weather", size: "s" },
        { id: "devices", size: "m" },
        { id: "nowPlaying", size: "s" },
        { id: "tonight", size: "s" },
        { id: "remoteAccess", size: "s" },
      ],
    ]);
    await startArranging(page);
    await page.keyboard.press("Escape");

    await test.info().attach("before resize", { body: await page.screenshot(), contentType: "image/png" });
    expect(await overlaps(page)).toEqual([]);

    for (const size of ["medium", "large", "small", "medium"]) {
      await page.getByRole("button", { name: `Show Weather ${size}` }).click();
      expect(await overlaps(page), `Weather at ${size}`).toEqual([]);
    }
    await test.info().attach("after resize", { body: await page.screenshot(), contentType: "image/png" });
  });

  test("moving a widget reflows its neighbours rather than covering them", async ({ page }) => {
    await page.setViewportSize({ width: 1150, height: 800 });
    await storeLayout(page, [
      [
        { id: "weather", size: "m" },
        { id: "devices", size: "m" },
        { id: "nowPlaying", size: "s" },
        { id: "tonight", size: "s" },
      ],
    ]);
    await startArranging(page);
    await page.keyboard.press("Escape");

    for (const card of ["Weather", "Devices", "Music"]) {
      await page.getByRole("button", { name: `Move ${card} down` }).click();
      expect(await overlaps(page), `after moving ${card} down`).toEqual([]);
    }
  });

  /** The panel scrolls; before, the dialog did, and the last rows slid off the panel's surface. */
  test("keeps every control of the arrange sheet on the sheet", async ({ page }) => {
    await page.setViewportSize({ width: 1024, height: 600 });
    await startArranging(page);

    const panel = page.locator(".dash__sheet .ink-sheet__panel");
    const last = panel.getByRole("button", { name: /Move Tonight to page/ }).last();
    await last.scrollIntoViewIfNeeded();

    const [panelBox, lastBox] = [await panel.boundingBox(), await last.boundingBox()];
    expect(panelBox && lastBox).toBeTruthy();
    if (!panelBox || !lastBox) return;
    expect(lastBox.y + lastBox.height).toBeLessThanOrEqual(panelBox.y + panelBox.height);
    expect(lastBox.y).toBeGreaterThanOrEqual(panelBox.y);

    const dialogScrolls = await page
      .locator("dialog.dash__sheet")
      .evaluate((d) => d.scrollHeight > d.clientHeight + 1);
    expect(dialogScrolls, "the dialog, not the panel, is what scrolls").toBe(false);
  });

  for (const theme of ["Light", "Dark"]) {
    test(`a disabled arrow still reads as an arrow (${theme})`, async ({ page }) => {
      await page.addInitScript((t) => localStorage.setItem("goosehub_theme", t), theme);
      await startArranging(page);

      // contrastOf reads colours only, so a fade would pass it: opacity is checked on its own.
      const opacity = (el: Element) => Number(getComputedStyle(el).opacity);

      // The first card's "up" is disabled in both the sheet and its own frame.
      const sheetUp = page.locator(".dash__sheet .dash__icon-btn:disabled").first();
      expect(await sheetUp.evaluate(opacity)).toBe(1);
      expect(await contrastOf(sheetUp)).toBeGreaterThanOrEqual(3);

      await page.keyboard.press("Escape");
      const frameUp = page.locator(".wframe__btn:disabled").first();
      expect(await frameUp.evaluate(opacity)).toBe(1);
      expect(await contrastOf(frameUp)).toBeGreaterThanOrEqual(3);
    });
  }
});

test.describe("the Extensions tab bar", () => {
  // Coral on Light is the reported case: the accent itself measures 2.53:1 on Jarida White.
  for (const [theme, accent] of [["Light", "Coral"], ["Light", "Purple"], ["Dark", "Purple"]]) {
    test(`shows the selected tab's label (${theme}, ${accent})`, async ({ page }) => {
      await page.addInitScript(
        ([t, a]) => {
          localStorage.setItem("goosehub_theme", t);
          localStorage.setItem("goosehub_accent", a);
        },
        [theme, accent],
      );
      await mockAllApiRoutes(page);
      await page.goto("/");
      await navigateTo(page, "Extensions");

      for (const name of [/installed/i, /tools/i, /browse/i]) {
        const tab = page.getByRole("tab", { name });
        await tab.click();
        await expect(tab).toHaveAttribute("aria-selected", "true");

        expect(await contrastOf(tab), `${name} label against its own background`).toBeGreaterThanOrEqual(4.5);
        // The hit-area pseudo-element is the full height of the tab; it must never paint.
        const hitArea = await tab.evaluate((el) => getComputedStyle(el, "::after").backgroundColor);
        expect(hitArea).toBe("rgba(0, 0, 0, 0)");
      }
      await test.info().attach("tab bar", {
        body: await page.locator(".ext-tab-bar").screenshot(),
        contentType: "image/png",
      });
    });
  }
});
