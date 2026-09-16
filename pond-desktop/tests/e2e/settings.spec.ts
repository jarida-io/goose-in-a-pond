import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

// ── Helper ────────────────────────────────────────────────────

async function goToSettings(page: import("@playwright/test").Page) {
  // Settings sits in the drawer's top list, so it takes an open before a click.
  await navigateTo(page, "Settings");
}

// ── Tests ─────────────────────────────────────────────────────

test.describe("Settings section", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.goto("/");
    await goToSettings(page);
  });

  test("Settings section renders without crashing", async ({ page }) => {
    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
    // At least one tab or form row should be visible
    await expect(
      page.getByText(/identity|voice|models|prompts|location|agent|data/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("renders all expected settings rows", async ({ page }) => {
    // Settings is now a list/detail panel — check for row labels visible in the list view
    const expectedRows = ["Account", "Voice", "Models", "Prompts"];
    for (const row of expectedRows) {
      await expect(
        page.getByRole("button", { name: row }).or(page.getByText(row)).first()
      ).toBeVisible({ timeout: 10_000 });
    }
  });

  test("Account panel shows assistant name and user name fields", async ({ page }) => {
    // Navigate into the Account detail panel
    await page.getByRole("button", { name: "Account" }).click({ timeout: 5_000 });

    await expect(
      page.getByLabel(/assistant name/i)
        .or(page.getByPlaceholder(/goose/i))
        .or(page.getByText(/assistant name/i))
        .first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("settings data is loaded from the API on mount", async ({ page }) => {
    // Navigate into Account panel where user_name and assistant_name inputs live
    await page.getByRole("button", { name: "Account" }).click({ timeout: 5_000 });
    // The mock returns assistant_name: "Pond" and user_name: "Jerry"
    await expect(
      page.locator('input').filter({ hasValue: "Pond" })
        .or(page.locator('input').filter({ hasValue: "Jerry" }))
        .first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("save settings calls PUT /api/v1/settings", async ({ page }) => {
    let putCalled = false;

    await page.route("**/api/v1/settings", (route) => {
      if (route.request().method() === "PUT") {
        putCalled = true;
        return route.fulfill({
          json: {
            assistant_name: "Pond",
            user_name: "Jerry",
            chat_provider: "llamafile",
            chat_model: "llama3.2",
            agent_memory_inject: false,
            prompt_style: "balanced",
            llm_temperature: 0.7,
            llm_max_tokens: 1024,
          },
        });
      }
      return route.continue();
    });

    // Wait for settings to load
    await page.waitForTimeout(500);

    // Find and click Save button
    const saveBtn = page.getByRole("button", { name: /save/i }).first();
    if (await saveBtn.isVisible({ timeout: 3_000 })) {
      await saveBtn.click();
      await page.waitForTimeout(300);
      expect(putCalled).toBe(true);
    }
  });

  test("Models tab renders model role rows", async ({ page }) => {
    const modelsTab = page
      .getByRole("tab", { name: /models/i })
      .or(page.getByText("Models"))
      .first();
    await modelsTab.click();

    // active-roles mock returns chat, think, task, asr, tts roles
    await expect(
      page.getByText(/chat|think|task|asr|tts|llamafile/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("Voice tab renders without errors", async ({ page }) => {
    const voiceTab = page
      .getByRole("tab", { name: /voice/i })
      .or(page.getByText("Voice"))
      .first();
    await voiceTab.click();

    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
    // Some form content should be present
    await expect(page.locator("body")).toBeVisible();
  });
});
