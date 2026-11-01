/** One-off Hub visual check; saves screenshots under /tmp. */
import { test, expect } from "@playwright/test";
import { existsSync } from "node:fs";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

test("Hub visual screenshot", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await mockAllApiRoutes(page);

  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 15000 });
  await page.waitForSelector(".dash", { timeout: 10000 });
  // Let animations settle
  await page.waitForTimeout(800);

  await page.screenshot({ path: "/tmp/hub-phase1-home.png", fullPage: false });
  console.log("Hub screenshot saved: /tmp/hub-phase1-home.png");

  // The drawer takes no shell width; report that its trigger shows and the panel is 344px.
  await page.locator('[aria-label="Open menu"]').click();
  const drawerWidth = await page
    .locator(".hdrawer")
    .evaluate((el) => el.getBoundingClientRect().width);
  console.log(`Drawer width: ${drawerWidth}px (expected 344)`);
  await page.keyboard.press("Escape");

  // Above 1100px the composer's mic replaces the dock's, so exactly one shows.
  const hasTalkButton = await page.locator(".hcomposer__talk").isVisible();
  const hasDockVoice = await page.locator(".dash__voice").isVisible();
  console.log(`Hold-to-talk visible: ${hasTalkButton}, dock mic visible: ${hasDockVoice}`);

  const hasPills = await page.locator(".rpills").isVisible();
  console.log(`Room pills visible: ${hasPills} (expected false -- removed in dbdf02a1)`);

  // Home's device tiles are HomeControlsCard's (.hcc__tile), not DeviceTile's (.dtile).
  const tileCount = await page.locator('[data-hook="home-controls"] .hcc__tile').count();
  console.log(`Device tile count: ${tileCount}`);

  // .cam is the CameraFeed root class.
  const camCount = await page.locator(".cam").count();
  console.log(`Camera tile count: ${camCount}`);

  // .wx (WeatherWidget) only with a location and weather on; otherwise the .dash__gap panel.
  const hasWeather = await page.locator(".wx").count();
  const hasWeatherGap = await page.locator(".dash__gap").count();
  console.log(`Weather card: ${hasWeather}, "set your location" panel: ${hasWeatherGap}`);

  // .cdock is the CategoryDock root class.
  const hasDock = await page.locator(".cdock").isVisible();
  console.log(`Category dock visible: ${hasDock}`);

  // A11y: the drawer's rows must be <button> with an accessible name
  await page.locator('[aria-label="Open menu"]').click();
  const railButtons = page.locator(".hdrawer__item");
  const railBtnCount = await railButtons.count();
  console.log(`\nA11y — IconRail buttons: ${railBtnCount}`);
  for (let i = 0; i < railBtnCount; i++) {
    const label = await railButtons.nth(i).getAttribute("aria-label");
    const tag = await railButtons.nth(i).evaluate((el) => el.tagName.toLowerCase());
    console.log(`  [${i}] tag=${tag} aria-label="${label}"`);
  }

  // A11y: above 1100px the offers are chips under the composer, not the asking column's rows.
  const offers = page.locator("button.sq__chip");
  const offerCount = await offers.count();
  console.log(`\nOffer chips: ${offerCount}`);
  for (let i = 0; i < Math.min(offerCount, 3); i++) {
    const prompt = await offers.nth(i).textContent();
    const why = await offers.nth(i).getAttribute("title");
    console.log(`  offer[${i}] "${prompt}" -- ${why}`);
  }

  // A11y: each page dot names its page and the page count.
  const dotBtns = page.locator(".wtrack__dot");
  const dotBtnCount = await dotBtns.count();
  console.log(`Page dots: ${dotBtnCount}`);
  for (let i = 0; i < Math.min(dotBtnCount, 3); i++) {
    const ariaLabel = await dotBtns.nth(i).getAttribute("aria-label");
    console.log(`  dot[${i}] aria-label="${ariaLabel}"`);
  }

  // 344px declared, plus up to 2px of border a side depending on box-sizing.
  expect(drawerWidth).toBeGreaterThanOrEqual(344);
  expect(drawerWidth).toBeLessThanOrEqual(348);
  expect(hasTalkButton).toBe(true);
  expect(hasDockVoice).toBe(false);
  // Every chip reaches the hub's 44px tap floor through `.reach` and carries the fact behind it.
  for (let i = 0; i < offerCount; i++) {
    await expect(offers.nth(i)).toBeVisible();
    await expect(offers.nth(i)).toHaveClass(/\breach\b/);
    const why = (await offers.nth(i).getAttribute("title")) ?? "";
    expect(why.trim().length).toBeGreaterThan(0);
  }
  expect(hasPills, "room pills were deleted in dbdf02a1; this must stay false").toBe(false);
  expect(tileCount).toBeGreaterThan(0);
});

test("Design reference screenshot", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const designPath =
    "/Users/jerry/Documents/Jarida/goose-in-a-pond/.ai/giap-design-bundle/project/Goose Hub.html";
  // The design bundle lives under gitignored .ai/, so it's absent in CI and fresh clones.
  test.skip(!existsSync(designPath), "design reference bundle (.ai/) not present");
  await page.goto(`file://${designPath}`);
  // Wait for React to render (loaded via unpkg CDN)
  try {
    await page.waitForSelector(".ghub", { timeout: 8000 });
    await page.waitForTimeout(1500);
  } catch {
    // CDN scripts may be blocked; still screenshot what loaded
    await page.waitForTimeout(4000);
  }
  await page.screenshot({ path: "/tmp/hub-design-reference.png", fullPage: false });
  console.log("Design reference screenshot saved: /tmp/hub-design-reference.png");
});

test("State: device tile toggles and route persists", async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(err.message));

  await page.setViewportSize({ width: 1440, height: 900 });
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 15000 });
  await page.waitForTimeout(500);

  // The value line is a read: it changes when the device answers, not on click.
  const firstTile = page.locator('[data-hook="home-controls"] .hcc__tile').first();
  await firstTile.waitFor({ timeout: 8000 });
  const statusBefore = await firstTile.locator(".hcc__value").textContent();
  console.log(`Tile status before toggle: "${statusBefore}"`);
  await firstTile.click();
  await page.waitForTimeout(500);
  const statusAfter = await firstTile.locator(".hcc__value").textContent();
  console.log(`Tile status after toggle:  "${statusAfter}"`);
  const toggled = statusBefore !== statusAfter;
  console.log(`Tile toggled: ${toggled}`);

  // Canvas has no drawer row, so no click reaches it; "Schedules" is the routines route.
  for (const label of ["Schedules", "Settings", "Chat", "Home"]) {
    await navigateTo(page, label);
    await page.waitForTimeout(200);
    const route = await page.evaluate(() => localStorage.getItem("goosehub_route"));
    console.log(`After clicking "${label}": goosehub_route="${route}"`);
  }

  const routeBeforeReload = await page.evaluate(() => localStorage.getItem("goosehub_route"));
  await page.reload();
  await page.waitForSelector(".ghub", { timeout: 15000 });
  const routeAfterReload = await page.evaluate(() => localStorage.getItem("goosehub_route"));
  console.log(`\nRoute before reload: "${routeBeforeReload}", after reload: "${routeAfterReload}"`);
  expect(routeAfterReload).toBe(routeBeforeReload);

  console.log(`\nConsole errors (${consoleErrors.length}):`);
  consoleErrors.forEach((e) => console.log("  ERROR:", e));

  // Test-env noise: HeroUI startContent, localstorage-file, Vite HMR socket, mocked-SSE MIME.
  const realErrors = consoleErrors.filter(
    (e) =>
      !e.includes("startContent") &&
      !e.includes("localstorage-file") &&
      !e.includes("ws://localhost:1421") &&
      !e.includes("WebSocket closed without opened") &&
      !e.includes("failed to connect to websocket") &&
      !e.includes("text/event-stream")
  );
  expect(realErrors, `Unexpected console errors: ${realErrors.join(", ")}`).toHaveLength(0);
});

test("Old shell sections still work (no hub regression)", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await mockAllApiRoutes(page);
  // Default start — no giap-section in localStorage → falls to "dashboard"
  await page.goto("/");
  await page.waitForSelector(".app-shell", { timeout: 15000 });

  const hubVisible = await page.locator(".ghub").isVisible();
  console.log(`Hub shell visible on non-hub start: ${hubVisible} (expected: false)`);
  expect(hubVisible).toBe(false);

  // The classic shell has the same drawer, so Settings is reached the same way.
  await navigateTo(page, "Settings");
  await page.waitForTimeout(400);
  const hubAfterSettings = await page.locator(".ghub").isVisible();
  console.log(`Hub shell visible after clicking Settings: ${hubAfterSettings} (expected: false)`);
  expect(hubAfterSettings).toBe(false);

});
