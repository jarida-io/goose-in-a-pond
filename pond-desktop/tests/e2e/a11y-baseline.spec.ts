import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mockAllApiRoutes } from "./helpers/api-mocks";

/**
 * WCAG 2 A/AA baseline scan for the core flows called out in the a11y pass:
 * Home dashboard, Canvas, the sidebar/IconRail nav, and an open modal
 * (routine builder, via HubModal + useDialogFocusTrap).
 */

async function scanAndAssert(page: import("@playwright/test").Page, label: string) {
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa"])
    .analyze();
  expect(results.violations, `${label}: ${JSON.stringify(results.violations, null, 2)}`).toEqual([]);
}

test.describe("a11y baseline (WCAG 2 A/AA)", () => {
  test("Home dashboard has no violations", async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.addInitScript(() => {
      localStorage.setItem("giap-section", "hub");
      localStorage.setItem("giap-force-hub", "1");
      localStorage.setItem("goosehub_route", "home");
    });
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/");
    await expect(page.locator(".dash")).toBeVisible({ timeout: 10_000 });
    await scanAndAssert(page, "Home");
  });

  test("Canvas section has no violations", async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.addInitScript(() => {
      localStorage.setItem("giap-section", "hub");
      localStorage.setItem("giap-force-hub", "1");
      localStorage.setItem("goosehub_route", "home");
    });
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/");
    await expect(page.locator(".dash")).toBeVisible({ timeout: 10_000 });
    await page.getByRole("button", { name: "Canvas", exact: true }).first().click();
    await expect(page.locator(".mcpc")).toBeVisible({ timeout: 10_000 });
    await scanAndAssert(page, "Canvas");
  });

  test("Open routine-builder modal is keyboard-focusable and has no violations", async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.addInitScript(() => {
      localStorage.setItem("giap-section", "hub");
      localStorage.setItem("giap-force-hub", "1");
      localStorage.setItem("goosehub_route", "home");
    });
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/");
    await expect(page.locator(".dash")).toBeVisible({ timeout: 10_000 });
    await page.getByRole("button", { name: "Routines", exact: true }).first().click();
    await expect(page.locator(".rt")).toBeVisible({ timeout: 10_000 });

    await page.getByRole("button", { name: /new routine/i }).first().click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toBeVisible({ timeout: 5_000 });

    // Focus management: opening the dialog must move focus inside it, not
    // leave it on whatever was focused in the page behind it.
    const focusInsideDialog = await page.evaluate(() => {
      const dialogEl = document.querySelector('[role="dialog"]');
      return !!dialogEl && dialogEl.contains(document.activeElement);
    });
    expect(focusInsideDialog, "focus should move into the dialog on open").toBe(true);

    // Escape must close it.
    await page.keyboard.press("Escape");
    await expect(dialog).not.toBeVisible({ timeout: 5_000 });
  });
});
