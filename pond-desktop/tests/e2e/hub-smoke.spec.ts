import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

test("Hub shell renders", async ({ page }) => {
  await mockAllApiRoutes(page);
  // A persisted "hub" is coerced to "dashboard" unless giap-force-hub is set; no UI entry exists.
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");

  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".irail")).toBeVisible();

  await expect(page.locator(".dash")).toBeVisible();
});

test("Hub rail navigation works", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");

  await expect(page.locator(".ghub"), "Hub shell renders").toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".dash"), "Home view renders").toBeVisible();

  await page.getByRole("button", { name: "Routines" }).click();
  await expect(page.locator(".view-title")).toHaveText("Routines");

  await page.getByRole("button", { name: "Settings" }).click();
  await expect(page.locator(".view-title")).toHaveText("Settings");

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

  await page.getByRole("button", { name: "Canvas" }).click();
  await expect(page.locator(".view-title")).toHaveText("Canvas");

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

  // First tile: the Driveway Light, which starts Off.
  const drivewayTile = page.locator(".dtile").first();
  await expect(drivewayTile.locator(".dtile__status")).toHaveText("Off", { timeout: 5000 });
  // Click the body, below the header row's dots button.
  await drivewayTile.click({ position: { x: 60, y: 80 } });
  await expect(drivewayTile.locator(".dtile__status")).toContainText("On", { timeout: 5000 });
});
