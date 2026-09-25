import { test } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

interface ButtonInfo {
  view: string;
  text: string;
  effect: string; // 'route-change' | 'dom-change' | 'console' | 'no-op' | 'error'
}

/**
 * The five views the icon rail used to list, and the drawer row that reaches
 * each one now.
 *
 * Two changed name rather than moving: the rail's "Goose" is the drawer's
 * "Chat" under Pond, and the rail's "Routines" is the "Schedules" chip under
 * Manage, which the hub renders as its routines route.
 *
 * Canvas has no drawer row at all -- it is a hidden section, reached in the hub
 * from a notification's "View on Canvas" -- so it is taken by its persisted
 * route instead. That is done here rather than in helpers/nav.ts because it is
 * not navigation through the drawer and does not belong in a shared helper.
 */
const VIEWS: Array<{ view: string; drawer: string | null }> = [
  { view: "Home",     drawer: "Home" },
  { view: "Goose",    drawer: "Chat" },
  { view: "Routines", drawer: "Schedules" },
  { view: "Settings", drawer: "Settings" },
  { view: "Canvas",   drawer: null },
];

test.setTimeout(180_000);
test("Hub dead-button audit", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });

  const inventory: Array<{ view: string; text: string; hasOnClick: boolean; cls: string }> = [];

  for (const { view: viewName, drawer } of VIEWS) {
    if (drawer) {
      await navigateTo(page, drawer);
    } else {
      // Canvas. A second init script wins over the first, so this re-opens the
      // app on the canvas route rather than on home. Canvas is last in VIEWS
      // for that reason: it is the only step that reloads.
      await page.addInitScript(() => {
        localStorage.setItem("goosehub_route", "canvas");
      });
      await page.goto("/");
      await page.waitForSelector(".ghub", { timeout: 10_000 });
    }
    await page.waitForTimeout(300);

    const buttons = await page.evaluate(() => {
      const out: Array<{ text: string; hasOnClick: boolean; cls: string }> = [];
      const main = document.querySelector(".ghub__main");
      if (!main) return out;
      main.querySelectorAll("button").forEach((b) => {
        const text = (b.textContent || "").trim().slice(0, 50) || "(no text)";
        // React attaches event handlers via __reactProps$… - look for it
        const propsKey = Object.keys(b).find((k) => k.startsWith("__reactProps$"));
        const props = propsKey ? (b as unknown as Record<string, { onClick?: unknown }>)[propsKey] : undefined;
        const hasOnClick = !!(props && props.onClick);
        out.push({ text, hasOnClick, cls: b.className.slice(0, 40) });
      });
      return out;
    });

    for (const b of buttons) inventory.push({ view: viewName, ...b });
  }

  const dead = inventory.filter((b) => !b.hasOnClick);
  // eslint-disable-next-line no-console
  console.log("BUTTON_INVENTORY_TOTAL=" + inventory.length);
  console.log("DEAD_BUTTONS=" + JSON.stringify(dead, null, 0));
});
