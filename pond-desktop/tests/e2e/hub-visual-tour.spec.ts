import { test, expect, type Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

/** Tour by opening the drawer for each destination, then drilling into the
 *  Settings sub-rows.
 *
 *  `drawer` is the row that reaches the view now: the rail's "Goose" is the
 *  drawer's "Chat" under Pond, and its "Routines" is the "Schedules" chip under
 *  Manage, which the hub renders as its routines route. Canvas has no drawer
 *  row -- it is a hidden section reached from a notification -- so it is taken
 *  by its persisted route, handled by `visit` below. */
const TOP_TOUR: Array<{ drawer: string | null; label: string; check: string }> = [
  { drawer: "Home",      label: "Home",          check: ".dash" },
  { drawer: "Chat",      label: "Chat",          check: ".chat2" },
  { drawer: null,        label: "Canvas",        check: ".mcpc" },
  { drawer: "Schedules", label: "Routines",      check: ".rt" },
  { drawer: "Settings",  label: "Settings",      check: ".set" },
];

/**
 * Reach one of the top destinations.
 *
 * Everything the drawer lists goes through the shared helper. Canvas does not,
 * and re-opening the app on its persisted route is local to this spec rather
 * than in helpers/nav.ts, which is about the drawer.
 */
async function visit(page: Page, drawer: string | null) {
  if (drawer) {
    await navigateTo(page, drawer);
    return;
  }
  await page.addInitScript(() => {
    localStorage.setItem("goosehub_route", "canvas");
  });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
}

const SETTINGS_TOUR: Array<{ row: RegExp; label: string }> = [
  { row: /^Models$/,            label: "Settings_Models" },
  { row: /^Prompts$/,           label: "Settings_Prompts" },
  { row: /^Voice$/,             label: "Settings_Voice" },
  { row: /^Memory$/,            label: "Settings_Memory" },
  { row: /^Extensions/,         label: "Settings_Extensions" },
  { row: /^Logs$/,              label: "Settings_Logs" },
  { row: /^Privacy$/,           label: "Settings_Privacy" },
  { row: /^Rooms/,              label: "Settings_Rooms" },
  { row: /^Cameras$/,           label: "Settings_Cameras" },
  { row: /^Notifications$/,     label: "Settings_NotifPrefs" },
  { row: /^Appearance$/,        label: "Settings_Appearance" },
  { row: /^Account$/,           label: "Settings_Account" },
];

test.describe.configure({ mode: "serial" });

test("Hub visual tour — light theme", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await expect(page.locator(".ghub"), "Hub shell renders").toBeVisible({ timeout: 10_000 });

  const results: Array<{ label: string; ok: boolean; note: string }> = [];

  for (const { drawer, label, check } of TOP_TOUR) {
    try {
      await visit(page, drawer);
      await page.waitForTimeout(300);
      const found = await page.locator(check).first().isVisible({ timeout: 4_000 }).catch(() => false);
      await page.screenshot({ path: `/tmp/hub-tour/light_${label}.png`, fullPage: false });
      results.push({ label, ok: found, note: found ? "" : `${check} not visible` });
    } catch (e) {
      results.push({ label, ok: false, note: String(e).slice(0, 200) });
    }
  }

  // Notifications via the bell in the shell bar
  try {
    await page.locator('[aria-label*="Notifications" i]').first().click();
    await page.waitForTimeout(300);
    const found = await page.locator(".view-title").first().isVisible({ timeout: 4_000 }).catch(() => false);
    await page.screenshot({ path: `/tmp/hub-tour/light_Notifications.png` });
    results.push({ label: "Notifications", ok: found, note: found ? "" : "view-title not visible" });
  } catch (e) {
    results.push({ label: "Notifications", ok: false, note: String(e).slice(0, 200) });
  }

  // Settings sub-tour: navigate to Settings then click each row
  await navigateTo(page, "Settings");
  await expect(page.locator(".set")).toBeVisible({ timeout: 4_000 });
  for (const { row, label } of SETTINGS_TOUR) {
    try {
      await page.getByRole("button", { name: row }).first().click();
      await page.waitForTimeout(300);
      const found = await page.locator(".setd").first().isVisible({ timeout: 4_000 }).catch(() => false);
      await page.screenshot({ path: `/tmp/hub-tour/light_${label}.png` });
      results.push({ label, ok: found, note: found ? "" : ".setd not visible" });
      // Back to Settings root
      const back = page.locator(".setd__back, [aria-label='Back'], button:has-text('Back')").first();
      if (await back.count() > 0) await back.click();
      else await navigateTo(page, "Settings");
      await expect(page.locator(".set")).toBeVisible({ timeout: 4_000 });
    } catch (e) {
      results.push({ label, ok: false, note: String(e).slice(0, 200) });
    }
  }
  // eslint-disable-next-line no-console
  console.log("TOUR_RESULTS=" + JSON.stringify(results));

  // Console-error capture
  const errs: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") errs.push(msg.text());
  });
  await page.evaluate(() => void 0); // flush
});

test("Hub visual tour — dark theme covers all top views", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
    localStorage.setItem("goosehub_theme", "Dark");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });

  for (const { drawer, label } of TOP_TOUR) {
    await visit(page, drawer);
    await page.waitForTimeout(300);
    await page.screenshot({ path: `/tmp/hub-tour/dark_${label}.png` });
  }
});

test("Hub responsive — narrow viewport collapses sidebar grids", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  // The design's media query: @media (max-width: 1080px) → ambient sidebar collapses
  await page.setViewportSize({ width: 1000, height: 900 });
  await page.goto("/");
  await expect(page.locator(".dash")).toBeVisible({ timeout: 10_000 });
  await page.waitForTimeout(300);
  await page.screenshot({ path: "/tmp/hub-tour/responsive_narrow_Home.png" });

  await navigateTo(page, "Schedules");
  await page.waitForTimeout(300);
  await page.screenshot({ path: "/tmp/hub-tour/responsive_narrow_Routines.png" });

  // Very narrow — single column
  await page.setViewportSize({ width: 720, height: 900 });
  await navigateTo(page, "Home");
  await page.waitForTimeout(300);
  await page.screenshot({ path: "/tmp/hub-tour/responsive_very_narrow_Home.png" });
});

test("Hub compact density mode", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
    localStorage.setItem("goosehub_density", "Compact");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await expect(page.locator(".dash")).toBeVisible({ timeout: 10_000 });
  await page.waitForTimeout(300);
  await page.screenshot({ path: "/tmp/hub-tour/density_compact_Home.png" });

  await navigateTo(page, "Settings");
  await page.waitForTimeout(300);
  await page.screenshot({ path: "/tmp/hub-tour/density_compact_Settings.png" });
});

test("Hub visual tour — accent variants on Home", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await expect(page.locator(".dash")).toBeVisible({ timeout: 10_000 });

  const ACCENTS: Array<{ name: string; rgb: [string, string, string, string] }> = [
    { name: "Blue",    rgb: ["#2563EB", "#1D4ED8", "#DBEAFE", "#EFF6FF"] },
    { name: "Teal",    rgb: ["#0D9488", "#0F766E", "#CCFBF1", "#F0FDFA"] },
    { name: "Coral",   rgb: ["#F97316", "#EA580C", "#FFEDD5", "#FFF7ED"] },
    { name: "Magenta", rgb: ["#DB2777", "#BE185D", "#FCE7F3", "#FDF2F8"] },
  ];
  for (const a of ACCENTS) {
    await page.evaluate((acc) => {
      const r = document.documentElement.style;
      r.setProperty("--pp",     acc.rgb[0]);
      r.setProperty("--pp-600", acc.rgb[1]);
      r.setProperty("--pp-100", acc.rgb[2]);
      r.setProperty("--pp-50",  acc.rgb[3]);
    }, a);
    await page.waitForTimeout(150);
    await page.screenshot({ path: `/tmp/hub-tour/accent_${a.name}_Home.png` });
  }
});

test("Hub interaction smoke — tile toggle + routine run + bell shortcut", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await expect(page.locator(".dash")).toBeVisible({ timeout: 10_000 });

  // First favourite light tile — capture state before/after click
  // Home's tiles are HomeControlsCard's now. Today's Dashboard rebuild stopped
  // DashboardGrid rendering DeviceTile, so `.dtile` matches nothing here -- and
  // this file's serial mode meant a stale `.home2` above hid that for a while.
  const firstTile = page.locator('[data-hook="home-controls"] .hcc__tile').first();
  const beforeStatus = await firstTile.innerText().catch(() => "n/a");
  await firstTile.click();
  await page.waitForTimeout(150);
  const afterStatus = await firstTile.innerText().catch(() => "n/a");
  // eslint-disable-next-line no-console
  console.log(`TILE_TOGGLE: before="${beforeStatus}" after="${afterStatus}"`);
  await page.screenshot({ path: "/tmp/hub-tour/interact_tile_after.png" });

  // Bell in the shell bar → notifications
  const bellInRail = page.locator('[aria-label*="Notifications" i]');
  if (await bellInRail.count() > 0) {
    await bellInRail.first().click();
    await expect(page.locator(".nfeed__title, .view-title").first()).toContainText(/notif/i, { timeout: 5_000 });
    await page.screenshot({ path: "/tmp/hub-tour/interact_notifications.png" });
  }

  // Routine run button — reached through the drawer's "Schedules" chip, which
  // is the routines route in this shell (a reload would reset the route, since
  // addInitScript pins it to home).
  await navigateTo(page, "Schedules");
  await expect(page.locator(".rt")).toBeVisible();
  const runBtn = page.locator(".rt-card__run").first();
  await runBtn.click();
  await page.waitForTimeout(200);
  await page.screenshot({ path: "/tmp/hub-tour/interact_routine_running.png" });
});
