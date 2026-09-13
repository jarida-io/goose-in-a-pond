import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

test("Hub shell renders from Settings preview button", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.goto("/");

  // Navigate to Settings
  await page.getByRole("button", { name: /settings/i }).first().click();

  // Click "Preview Goose Hub redesign"
  await page.getByRole("button", { name: /preview goose hub redesign/i }).click();

  // Hub rail should be visible
  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".irail")).toBeVisible();

  // Home view content
  await expect(page.locator(".dash")).toBeVisible();
  await expect(page.locator(".askgoose")).toBeVisible();
  await expect(page.locator(".rpills")).toBeVisible();
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

  // Navigate to Routines
  await page.getByRole("button", { name: "Routines" }).click();
  await expect(page.locator(".view-title")).toHaveText("Routines");

  // Navigate to Settings
  await page.getByRole("button", { name: "Settings" }).click();
  await expect(page.locator(".view-title")).toHaveText("Settings");

  // Back to Home
  await page.getByRole("button", { name: "Home" }).click();
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

  // Navigate to Canvas
  await page.getByRole("button", { name: "Canvas" }).click();
  await expect(page.locator(".view-title")).toHaveText("Canvas");

  // localStorage should have canvas as route
  const stored = await page.evaluate(() => localStorage.getItem("goosehub_route"));
  expect(stored).toBe("canvas");
});

test("Device tile toggles state in place", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");

  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".dtile").first()).toBeVisible({ timeout: 8_000 });

  // Click the Driveway Light tile (first tile, starts Off)
  const drivewayTile = page.locator(".dtile").first();
  // Verify it starts as Off — wait for tile to fully hydrate
  await expect(drivewayTile.locator(".dtile__status")).toHaveText("Off", { timeout: 5000 });
  // Click in the centre of the tile body (below the header row with the dots button)
  await drivewayTile.click({ position: { x: 60, y: 80 } });
  // After toggle, should show On with brightness
  await expect(drivewayTile.locator(".dtile__status")).toContainText("On", { timeout: 5000 });
});
