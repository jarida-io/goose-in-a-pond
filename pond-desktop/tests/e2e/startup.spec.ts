import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

test.beforeEach(async ({ page }) => {
  await mockAllApiRoutes(page);
});

test.describe("App startup", () => {
  test("app loads and renders the sidebar", async ({ page }) => {
    await page.goto("/");
    // <aside aria-label="Navigation"> — ARIA role is "complementary" for <aside>
    await expect(page.locator('aside[aria-label="Navigation"]')).toBeVisible({ timeout: 10_000 });
  });

  test("sidebar shows core navigation items", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator('aside[aria-label="Navigation"]')).toBeVisible({ timeout: 10_000 });
    // Collapsed: title attributes; expanded: text labels. Either will do.
    await expect(
      page.getByRole("button").filter({ hasText: /dashboard|home/i }).or(
        page.locator('[title="Dashboard"], [title="Home"]')
      ).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("server status indicator is visible", async ({ page }) => {
    await page.goto("/");
    // The status dot on the avatar.
    await expect(page.locator('.sidebar__avatar-dot').first()).toBeVisible();
  });

  test("Voice mode button is present in sidebar footer", async ({ page }) => {
    await page.goto("/");
    await expect(
      page.locator('[aria-label="Voice mode"]').first()
    ).toBeVisible();
  });

  test("Settings section loads without errors", async ({ page }) => {
    await page.goto("/");

    const settingsBtn = page.getByRole("button", { name: /settings/i }).first();
    await settingsBtn.click();

    await expect(
      page.getByRole("button", { name: "Account" })
        .or(page.getByRole("button", { name: "Models" }))
        .first()
    ).toBeVisible({ timeout: 10_000 });

    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });

  test("Dashboard section renders without errors", async ({ page }) => {
    await page.goto("/");

    const dashBtn = page
      .getByRole("button")
      .filter({ hasText: /dashboard/i })
      .or(page.locator('[title="Dashboard"]'))
      .first();

    if (await dashBtn.isVisible()) {
      await dashBtn.click();
    }

    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });

  test("page title is present", async ({ page }) => {
    await page.goto("/");
    const title = await page.title();
    expect(title.length).toBeGreaterThan(0);
  });
});
