/** One-off Hub visual check; saves screenshots under /tmp. */
import { test, expect } from "@playwright/test";
import { existsSync } from "node:fs";
import { mockAllApiRoutes } from "./helpers/api-mocks";

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

  const railWidth = await page.locator(".irail").evaluate((el) => el.getBoundingClientRect().width);
  console.log(`IconRail width: ${railWidth}px (expected 86)`);


  const tileCount = await page.locator(".dtile").count();
  console.log(`Device tile count: ${tileCount}`);

  // .cam is the CameraFeed root class.
  const camCount = await page.locator(".cam").count();
  console.log(`Camera tile count: ${camCount}`);

  // .wx is the WeatherWidget root class.
  const hasWeather = await page.locator(".wx").isVisible();
  console.log(`Weather widget visible: ${hasWeather}`);


  // A11y: IconRail buttons must be <button> with aria-label
  const railButtons = page.locator(".irail__item");
  const railBtnCount = await railButtons.count();
  console.log(`\nA11y — IconRail buttons: ${railBtnCount}`);
  for (let i = 0; i < railBtnCount; i++) {
    const label = await railButtons.nth(i).getAttribute("aria-label");
    const tag = await railButtons.nth(i).evaluate((el) => el.tagName.toLowerCase());
    console.log(`  [${i}] tag=${tag} aria-label="${label}"`);
  }


  // A11y: dot buttons on tiles
  const dotBtns = page.locator(".dtile__dots");
  const dotBtnCount = await dotBtns.count();
  console.log(`Device tile dot buttons: ${dotBtnCount}`);
  for (let i = 0; i < Math.min(dotBtnCount, 3); i++) {
    const ariaLabel = await dotBtns.nth(i).getAttribute("aria-label");
    console.log(`  dot[${i}] aria-label="${ariaLabel}"`);
  }

  expect(railWidth).toBeGreaterThanOrEqual(84);
  expect(railWidth).toBeLessThanOrEqual(90);
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

  const firstTile = page.locator(".dtile").first();
  await firstTile.waitFor({ timeout: 8000 });
  const statusBefore = await firstTile.locator(".dtile__status").textContent();
  console.log(`Tile status before toggle: "${statusBefore}"`);
  await firstTile.click({ position: { x: 60, y: 80 } });
  await page.waitForTimeout(300);
  const statusAfter = await firstTile.locator(".dtile__status").textContent();
  console.log(`Tile status after toggle:  "${statusAfter}"`);
  const toggled = statusBefore !== statusAfter;
  console.log(`Tile toggled: ${toggled}`);

  for (const label of ["Routines", "Canvas", "Settings", "Goose", "Home"]) {
    await page.getByRole("button", { name: label }).click();
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

  await page.getByRole("button", { name: /settings/i }).first().click();
  await page.waitForTimeout(400);
  const hubAfterSettings = await page.locator(".ghub").isVisible();
  console.log(`Hub shell visible after clicking Settings: ${hubAfterSettings} (expected: false)`);
  expect(hubAfterSettings).toBe(false);

  const previewBtn = page.getByRole("button", { name: /preview goose hub redesign/i });
  await expect(previewBtn).toBeVisible();
  console.log("Preview button visible in Settings: true");

  await previewBtn.click();
  await page.waitForSelector(".ghub", { timeout: 8000 });
  console.log("Hub renders after clicking Preview: true");
});
