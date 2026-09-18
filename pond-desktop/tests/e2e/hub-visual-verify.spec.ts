/**
 * One-off visual verification spec for Hub Phase 1.
 * Run: npx playwright test tests/e2e/hub-visual-verify.spec.ts
 * Generates screenshots at /tmp/hub-phase1-home.png and /tmp/hub-design-reference.png.
 */
import { test, expect } from "@playwright/test";
import { existsSync } from "node:fs";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

test("Hub visual screenshot", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await mockAllApiRoutes(page);

  // Land directly in hub by pre-setting localStorage
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

  // Navigation is a drawer now, so the shell spends no width on it. What is
  // worth reporting is that its trigger is on screen and the panel is 344px.
  await page.locator('[aria-label="Open menu"]').click();
  const drawerWidth = await page
    .locator(".hdrawer")
    .evaluate((el) => el.getBoundingClientRect().width);
  console.log(`Drawer width: ${drawerWidth}px (expected 344)`);
  await page.keyboard.press("Escape");

  // Check AskGoose bar present
  // The AskGoose bar is gone -- deleted in dbdf02a1, "delete the components the
  // Home redesign left behind". Its job is now two round controls at the foot
  // of the screen: `.dash__voice` starts a voice turn, and the left column
  // offers questions you can tap. This assertion was left behind when the rest
  // of this file was migrated to the drawer, so it has been failing on a
  // selector that no longer exists in `src/` at all.
  const hasVoiceButton = await page.locator(".dash__voice").isVisible();
  console.log(`Talk-to-Goose button visible: ${hasVoiceButton}`);

  // Room pills went the same way as the AskGoose bar, in the same commit
  // (dbdf02a1). Home is three arrangeable cards and an asking column now; there
  // is no room filter on it, so there is nothing here to assert. Reported for
  // the log rather than deleted outright, because a reader comparing this file
  // against an old screenshot should be told where they went.
  const hasPills = await page.locator(".rpills").isVisible();
  console.log(`Room pills visible: ${hasPills} (expected false -- removed in dbdf02a1)`);

  // Home's device tiles are HomeControlsCard's now; DeviceTile (.dtile) still
  // ships, in the Devices section and in chat's ResultCard, but not here.
  const tileCount = await page.locator('[data-hook="home-controls"] .hcc__tile').count();
  console.log(`Device tile count: ${tileCount}`);

  // Check camera grid (.cam is the CameraFeed root class)
  const camCount = await page.locator(".cam").count();
  console.log(`Camera tile count: ${camCount}`);

  // The weather card (.wx is the WeatherWidget root class) is drawn only when
  // the pond has a location and weather turned on; otherwise Home draws the
  // panel that asks for one, in its place.
  const hasWeather = await page.locator(".wx").count();
  const hasWeatherGap = await page.locator(".dash__gap").count();
  console.log(`Weather card: ${hasWeather}, "set your location" panel: ${hasWeatherGap}`);

  // Check category dock at bottom (.cdock is the CategoryDock root class)
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

  // A11y: the asking column. It carries either the proposal that is waiting or
  // the questions the pond can currently answer, and on a pond with neither it
  // carries one sentence. All three states are legitimate, so this reports
  // rather than asserts -- what it pins is that the column EXISTS.
  const offers = page.locator(".sq__offer");
  const offerCount = await offers.count();
  const quiet = await page.locator(".sq__quiet").count();
  console.log(`\nAsking column -- offers: ${offerCount}, quiet line: ${quiet > 0}`);
  for (let i = 0; i < Math.min(offerCount, 3); i++) {
    const prompt = await offers.nth(i).locator(".sq__offer-prompt").textContent();
    const why = await offers.nth(i).locator(".sq__offer-why").textContent();
    console.log(`  offer[${i}] "${prompt}" -- ${why}`);
  }

  // A11y: the track's page dots. Every one names the page it goes to and the
  // number of pages there are, because "dot 2" tells nobody anything.
  const dotBtns = page.locator(".wtrack__dot");
  const dotBtnCount = await dotBtns.count();
  console.log(`Page dots: ${dotBtnCount}`);
  for (let i = 0; i < Math.min(dotBtnCount, 3); i++) {
    const ariaLabel = await dotBtns.nth(i).getAttribute("aria-label");
    console.log(`  dot[${i}] aria-label="${ariaLabel}"`);
  }

  // The 86px rail this used to measure no longer exists; the drawer panel is
  // what carries the navigation's width now. 344px is the declared width, plus
  // up to 2px of border on each side depending on box-sizing.
  expect(drawerWidth).toBeGreaterThanOrEqual(344);
  expect(drawerWidth).toBeLessThanOrEqual(348);
  expect(hasVoiceButton).toBe(true);
  // Every offer is tappable at the hub's 44px floor, and carries the fact that
  // produced it -- an offer with no reason is the template the engine exists
  // not to be.
  for (let i = 0; i < offerCount; i++) {
    const box = await offers.nth(i).boundingBox();
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(44);
    const why = (await offers.nth(i).locator(".sq__offer-why").textContent()) ?? "";
    expect(why.trim().length).toBeGreaterThan(0);
  }
  expect(hasPills, "room pills were deleted in dbdf02a1; this must stay false").toBe(false);
  expect(tileCount).toBeGreaterThan(0);
});

test("Design reference screenshot", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const designPath =
    "/Users/jerry/Documents/Jarida/goose-in-a-pond/.ai/giap-design-bundle/project/Goose Hub.html";
  // Dev-only visual reference: the design bundle lives under .ai/ (gitignored), so
  // it's absent in CI / fresh clones. Skip rather than fail.
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

  // Toggle a device tile. The value line is a READ, so it changes when the
  // device answers rather than when the click lands.
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

  // Navigate through the drawer's destinations and check the route persists.
  // "Schedules" is the routines route and "Chat" is what the rail called Goose.
  // Canvas has dropped out of the crawl: it is a hidden section with no drawer
  // row, so there is no click that reaches it, and reaching it by its persisted
  // route would be asserting that localStorage holds what this test just wrote
  // into localStorage.
  for (const label of ["Schedules", "Settings", "Chat", "Home"]) {
    await navigateTo(page, label);
    await page.waitForTimeout(200);
    const route = await page.evaluate(() => localStorage.getItem("goosehub_route"));
    console.log(`After clicking "${label}": goosehub_route="${route}"`);
  }

  // Reload and check route restores
  const routeBeforeReload = await page.evaluate(() => localStorage.getItem("goosehub_route"));
  await page.reload();
  await page.waitForSelector(".ghub", { timeout: 15000 });
  const routeAfterReload = await page.evaluate(() => localStorage.getItem("goosehub_route"));
  console.log(`\nRoute before reload: "${routeBeforeReload}", after reload: "${routeAfterReload}"`);
  expect(routeAfterReload).toBe(routeBeforeReload);

  console.log(`\nConsole errors (${consoleErrors.length}):`);
  consoleErrors.forEach((e) => console.log("  ERROR:", e));

  // Fail on real app errors; filter test-environment noise:
  // - HeroUI startContent prop warning
  // - localstorage-file warning
  // - Vite HMR websocket (test env port conflict)
  // - EventSource MIME mismatch from mocked chat/stream endpoint
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

  // Hub shell must NOT be visible when in the classic shell
  const hubVisible = await page.locator(".ghub").isVisible();
  console.log(`Hub shell visible on non-hub start: ${hubVisible} (expected: false)`);
  expect(hubVisible).toBe(false);

  // Settings section should still render without the Hub. The classic shell
  // carries the same drawer, so it is reached the same way.
  await navigateTo(page, "Settings");
  await page.waitForTimeout(400);
  const hubAfterSettings = await page.locator(".ghub").isVisible();
  console.log(`Hub shell visible after clicking Settings: ${hubAfterSettings} (expected: false)`);
  expect(hubAfterSettings).toBe(false);

  // There is no "Preview Goose Hub redesign" button to click. This half of the
  // test outlived the control it named: the entry point survives only in
  // comments, and `hub-smoke.spec.ts` says so at the top of its own file. Every
  // hub test enters through `giap-force-hub` instead, which is what the
  // reducer's opt-in exists for.
  //
  // What this test is REALLY about -- that the classic shell does not
  // accidentally render the hub -- is asserted twice above and is untouched by
  // the missing button.
});
