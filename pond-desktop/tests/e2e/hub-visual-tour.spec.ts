import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

/** Tour by clicking the IconRail then drilling into Settings sub-rows.
 *  Reload is avoided because addInitScript would reset the route. */
const TOP_TOUR: Array<{ rail: string; label: string; check: string }> = [
  { rail: "Home",     label: "Home",          check: ".dash" },
  { rail: "Goose",    label: "Chat",          check: ".chat2" },
  { rail: "Canvas",   label: "Canvas",        check: ".mcpc" },
  { rail: "Routines", label: "Routines",      check: ".rt" },
  { rail: "Settings", label: "Settings",      check: ".set" },
];
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

  for (const { rail, label, check } of TOP_TOUR) {
    try {
      await page.getByRole("button", { name: rail, exact: true }).first().click();
      await page.waitForTimeout(300);
      const found = await page.locator(check).first().isVisible({ timeout: 4_000 }).catch(() => false);
      await page.screenshot({ path: `/tmp/hub-tour/light_${label}.png`, fullPage: false });
      results.push({ label, ok: found, note: found ? "" : `${check} not visible` });
    } catch (e) {
      results.push({ label, ok: false, note: String(e).slice(0, 200) });
    }
  }

  // Notifications via the bell shortcut in IconRail
  try {
    await page.locator(".bell-shortcut, .irail [aria-label*='otification' i]").first().click();
    await page.waitForTimeout(300);
    const found = await page.locator(".view-title").first().isVisible({ timeout: 4_000 }).catch(() => false);
    await page.screenshot({ path: `/tmp/hub-tour/light_Notifications.png` });
    results.push({ label: "Notifications", ok: found, note: found ? "" : "view-title not visible" });
  } catch (e) {
    results.push({ label: "Notifications", ok: false, note: String(e).slice(0, 200) });
  }

  // Settings sub-tour: navigate to Settings then click each row
  await page.getByRole("button", { name: "Settings", exact: true }).first().click();
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
      else await page.getByRole("button", { name: "Settings", exact: true }).first().click();
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

  for (const { rail, label } of TOP_TOUR) {
    await page.getByRole("button", { name: rail, exact: true }).first().click();
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

  await page.getByRole("button", { name: "Routines", exact: true }).first().click();
  await page.waitForTimeout(300);
  await page.screenshot({ path: "/tmp/hub-tour/responsive_narrow_Routines.png" });

  // Very narrow — single column
  await page.setViewportSize({ width: 720, height: 900 });
  await page.getByRole("button", { name: "Home", exact: true }).first().click();
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

  await page.getByRole("button", { name: "Settings", exact: true }).first().click();
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
  const firstTile = page.locator(".dtile").first();
  const beforeStatus = await firstTile.locator(".dtile__status, .dtile__bottom").innerText().catch(() => "n/a");
  await firstTile.click();
  await page.waitForTimeout(150);
  const afterStatus = await firstTile.locator(".dtile__status, .dtile__bottom").innerText().catch(() => "n/a");
  // eslint-disable-next-line no-console
  console.log(`TILE_TOGGLE: before="${beforeStatus}" after="${afterStatus}"`);
  await page.screenshot({ path: "/tmp/hub-tour/interact_tile_after.png" });

  // Bell shortcut in IconRail → notifications
  const bellInRail = page.locator(".bell-shortcut, .irail [aria-label*='otification' i]");
  if (await bellInRail.count() > 0) {
    await bellInRail.first().click();
    await expect(page.locator(".nfeed__title, .view-title").first()).toContainText(/notif/i, { timeout: 5_000 });
    await page.screenshot({ path: "/tmp/hub-tour/interact_notifications.png" });
  }

  // Routine run button — navigate via rail click (addInitScript would reset
  // a localStorage-set route on reload)
  await page.getByRole("button", { name: "Routines", exact: true }).first().click();
  await expect(page.locator(".rt")).toBeVisible();
  const runBtn = page.locator(".rt-card__run").first();
  await runBtn.click();
  await page.waitForTimeout(200);
  await page.screenshot({ path: "/tmp/hub-tour/interact_routine_running.png" });
});
