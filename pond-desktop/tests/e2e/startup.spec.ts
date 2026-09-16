import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

test.beforeEach(async ({ page }) => {
  await mockAllApiRoutes(page);
});

/**
 * Open the navigation drawer and return its "Go to" list.
 *
 * Scoped, not global: "Home" is both a destination and the name of a room, so
 * an unscoped getByRole matches two buttons and fails strict mode.
 */
async function openDrawer(page: import("@playwright/test").Page) {
  await page.locator('[aria-label="Open menu"]').click();
  await expect(page.locator('[role="dialog"][aria-label="Menu"]')).toBeVisible({
    timeout: 10_000,
  });
  return page.getByRole("navigation", { name: "Go to" });
}

test.describe("App startup", () => {
  test("app loads and renders the shell bar", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator('[aria-label="Open menu"]')).toBeVisible({ timeout: 10_000 });
  });

  test("the drawer holds the core destinations", async ({ page }) => {
    await page.goto("/");
    const goTo = await openDrawer(page);

    await expect(goTo.getByRole("button", { name: "Home", exact: true })).toBeVisible();
    await expect(goTo.getByRole("button", { name: "Settings", exact: true })).toBeVisible();
  });

  test("Manage holds the twelve sections the sidebar used to list", async ({ page }) => {
    await page.goto("/");
    const goTo = await openDrawer(page);

    // Collapsed by default: twelve chips would push Rooms off a 600px panel.
    await expect(goTo.getByRole("button", { name: "Devices", exact: true })).toHaveCount(0);

    await goTo.getByRole("button", { name: "Manage" }).click();
    for (const label of ["Devices", "Mesh", "Pairing", "Schedules", "Context", "Skills",
                         "Recipes", "Logs", "Models", "Prompts", "Extensions", "Faces"]) {
      await expect(goTo.getByRole("button", { name: label, exact: true })).toBeVisible();
    }
  });

  test("Voice is reachable under Pond", async ({ page }) => {
    await page.goto("/");
    const goTo = await openDrawer(page);
    await expect(goTo.getByRole("button", { name: "Voice", exact: true })).toBeVisible();
  });

  test("the drawer closes on Escape", async ({ page }) => {
    await page.goto("/");
    await openDrawer(page);
    await page.keyboard.press("Escape");
    await expect(page.locator('[role="dialog"][aria-label="Menu"]')).toHaveCount(0);
  });

  test("Settings section loads without errors", async ({ page }) => {
    await page.goto("/");
    const goTo = await openDrawer(page);
    await goTo.getByRole("button", { name: "Settings", exact: true }).click();

    // Settings shows a list/detail panel — the list rows should be visible
    await expect(
      page.getByRole("button", { name: "Account" })
        .or(page.getByRole("button", { name: "Models" }))
        .first()
    ).toBeVisible({ timeout: 10_000 });

    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });

  test("Dashboard section renders without errors", async ({ page }) => {
    await page.goto("/");
    const goTo = await openDrawer(page);
    await goTo.getByRole("button", { name: "Home", exact: true }).click();

    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });

  test("page title is present", async ({ page }) => {
    await page.goto("/");
    const title = await page.title();
    expect(title.length).toBeGreaterThan(0);
  });
});
