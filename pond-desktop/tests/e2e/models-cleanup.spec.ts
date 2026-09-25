/** Models "Free up space": posts cleanup, toasts the reclaimed bytes, refreshes disk usage. */
import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

async function goToModels(page: import("@playwright/test").Page) {
  await page.goto("/");
  const modelsBtn = page
    .getByRole("button", { name: /models/i })
    .or(page.locator('[title="Models"]'))
    .first();
  await modelsBtn.click({ timeout: 10_000 });
}

test.describe("Models cleanup", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("free-up-space button posts to /cleanup and shows reclaimed bytes", async ({ page }) => {
    let cleanupCalled = false;
    await page.route("**/api/v1/models/disk-usage", (route) =>
      route.fulfill({
        json: {
          total_bytes: 5_368_709_120,
          by_category: { gguf: 4_000_000_000, whisper: 500_000_000, tts: 100_000_000, embedding: 0, llamafile: 0 },
          hf_cache_bytes: 4_600_000_000,
          incomplete_bytes: 0,
        },
      }),
    );
    await page.route("**/api/v1/models/cleanup", (route) => {
      cleanupCalled = true;
      return route.fulfill({
        json: {
          reclaimed_bytes: 12_582_912, // 12 MB
          removed: [
            { path: "/data/hf_cache/hub/models--giap-local--foo/blobs/abc", category: "gguf", bytes: 12_582_912 },
          ],
        },
      });
    });

    await goToModels(page);

    const btn = page.getByTestId("models-cleanup-btn");
    await expect(btn).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId("models-disk-usage")).toContainText(/used/i);

    await btn.click();

    await expect(
      page.getByText(/reclaimed\s+12(\.\d+)?\s*mb/i).first(),
    ).toBeVisible({ timeout: 10_000 });
    expect(cleanupCalled).toBe(true);
  });

  test("button is disabled while a download is in progress", async ({ page }) => {
    await page.route("**/api/v1/models/disk-usage", (route) =>
      route.fulfill({
        json: { total_bytes: 0, by_category: {}, hf_cache_bytes: 0, incomplete_bytes: 0 },
      }),
    );
    await page.route("**/api/v1/models/download/progress", (route) =>
      route.fulfill({
        json: {
          downloads: [
            {
              filename: "gemma-4-E2B-it-Q4_K_M.gguf",
              category: "gguf",
              downloaded_bytes: 1_000_000,
              total_bytes: 3_000_000_000,
              status: "downloading",
              finished_at: null,
            },
          ],
        },
      }),
    );

    await goToModels(page);
    const btn = page.getByTestId("models-cleanup-btn");
    await expect(btn).toBeVisible({ timeout: 10_000 });
    await expect(btn).toBeDisabled();
  });
});
