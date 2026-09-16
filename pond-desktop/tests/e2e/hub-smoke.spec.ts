import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

// There is no "Preview Goose Hub redesign" button anywhere in src/ -- the
// comment in desktopState.ts that named one described a control that does not
// exist, and this test used to click it. It forces the hub through
// `giap-force-hub` like every other test in this file instead, which is what
// main did while this branch only skipped it.
test("Hub shell renders", async ({ page }) => {
  await mockAllApiRoutes(page);
  // The classic UI is the default and a persisted "hub" is coerced to
  // "dashboard" on launch, so the hub is entered the same way the two tests
  // below do it. There is no Settings button to click: the preview entry point
  // survives only in comments.
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");

  // Hub shell, and the drawer's trigger rather than a rail
  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator('[aria-label="Open menu"]')).toBeVisible();

  // Home view content. `DashboardGrid` is the body both surfaces render, so
  // this is `.dash` and not the hub's old `.home2`, which nothing emits.
  await expect(page.locator(".dash")).toBeVisible();
});

test("Hub rail navigation works", async ({ page }) => {
  await mockAllApiRoutes(page);
  // Pre-set route to hub
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");

  await expect(page.locator(".ghub"), "Hub shell renders").toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".dash"), "Home view renders").toBeVisible();

  // Routines is reached as "Schedules" -- the drawer speaks GuiSection, and the
  // hub maps that to its own "routines" route.
  await navigateTo(page, "Schedules");
  await expect(page.locator(".view-title")).toHaveText("Routines");

  await navigateTo(page, "Settings");
  await expect(page.locator(".view-title")).toHaveText("Settings");

  await navigateTo(page, "Home");
  await expect(page.locator(".dash")).toBeVisible();
});

test("Hub route persists to localStorage", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");

  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });

  // Canvas left the nav with the rail -- the design's drawer has no entry for
  // it -- so persistence is exercised through a destination that is still in
  // the list. Schedules is the interesting one: the drawer emits the GuiSection
  // and the hub stores its own route name, so this also pins that mapping.
  await navigateTo(page, "Schedules");
  await expect(page.locator(".view-title")).toHaveText("Routines");

  const stored = await page.evaluate(() => localStorage.getItem("goosehub_route"));
  expect(stored).toBe("routines");
});

/**
 * Home's device tiles are `HomeControlsCard` now, not `DeviceTile` — the old
 * `.dtile*` selectors moved to the Devices section, where that component still
 * ships.
 *
 * The assertion changed with the component. The tile no longer writes a value
 * optimistically and then hopes: it sends the switch, ASKS the device what
 * happened, and shows the answer. So what is asserted is the value line
 * changing after the click, which is the read landing, rather than a local
 * write appearing.
 */
test("Device tile toggles state in place", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");

  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  const tile = page.locator('[data-hook="home-controls"] .hcc__tile').first();
  await expect(tile).toBeVisible({ timeout: 8_000 });

  // The first read has to land before the click means anything: a tile that has
  // not been answered for is the "Not reporting" branch, which opens the sheet
  // rather than switching anything.
  await expect(tile.locator(".hcc__value")).toHaveText("Off", { timeout: 5000 });
  await tile.click();
  await expect(tile.locator(".hcc__value")).toHaveText("On", { timeout: 5000 });
});
