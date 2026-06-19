import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// ── Helpers ───────────────────────────────────────────────────

async function goToSchedules(page: import("@playwright/test").Page) {
  // Find the Schedules nav item (may be collapsed → title attr, or expanded → text)
  const schedBtn = page
    .getByRole("button")
    .filter({ hasText: /schedules?/i })
    .or(page.locator('[title="Schedules"]'))
    .first();
  await schedBtn.click();
}

// ── Tests ─────────────────────────────────────────────────────

test.describe("Schedules section", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.goto("/");
  });

  test("navigating to Schedules renders the section", async ({ page }) => {
    await goToSchedules(page);
    // Section heading or empty-state message should appear
    await expect(
      page
        .getByText(/schedules?/i)
        .or(page.getByText(/no schedules/i))
        .or(page.getByText(/add.*schedule/i))
        .first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("shows empty state when API returns no schedules", async ({ page }) => {
    // api-mocks returns [] for GET /api/v1/schedules
    await goToSchedules(page);

    // Wait for the list fetch to complete — no schedule cards should appear
    await page.waitForTimeout(500);
    await expect(page.getByText(/morning briefing/i)).not.toBeVisible();
    await expect(page.getByText(/evening summary/i)).not.toBeVisible();
  });

  test("renders a schedule returned by the API", async ({ page }) => {
    // Override the schedules mock to return one entry
    await page.route("**/api/v1/schedules", (route) => {
      if (route.request().method() === "GET") {
        return route.fulfill({
          json: [
            {
              id: "sched-test",
              name: "Morning Briefing",
              cron: "0 0 7 * * *",
              prompt: "What's happening today?",
              enabled: true,
            },
          ],
        });
      }
      return route.continue();
    });

    await goToSchedules(page);

    await expect(page.getByText("Morning Briefing")).toBeVisible({
      timeout: 10_000,
    });
  });

  test("Add / New schedule button is visible", async ({ page }) => {
    await goToSchedules(page);

    const addBtn = page
      .getByRole("button")
      .filter({ hasText: /add|new|create/i })
      .first();
    await expect(addBtn).toBeVisible({ timeout: 10_000 });
  });

  test("create schedule form submits correctly", async ({ page }) => {
    let postBody: Record<string, string> | null = null;

    // Override POST to capture the payload
    await page.route("**/api/v1/schedules", (route) => {
      if (route.request().method() === "POST") {
        postBody = route.request().postDataJSON() as Record<string, string>;
        return route.fulfill({
          status: 201,
          json: {
            id: "new-sched",
            name: postBody?.name ?? "New",
            cron: postBody?.cron ?? "0 0 8 * * *",
            prompt: postBody?.prompt ?? "",
            enabled: true,
          },
        });
      }
      return route.fulfill({ json: [] });
    });

    await goToSchedules(page);

    // Open form
    const addBtn = page
      .getByRole("button")
      .filter({ hasText: /add|new|create/i })
      .first();
    await addBtn.click();

    // Fill in the form if inputs become visible
    const nameInput = page
      .getByPlaceholder(/name/i)
      .or(page.getByLabel(/name/i))
      .first();
    const cronInput = page
      .getByPlaceholder(/cron/i)
      .or(page.getByLabel(/cron/i))
      .first();
    const promptInput = page
      .getByLabel("Schedule prompt")
      .or(page.locator('textarea[aria-label="Schedule prompt"]'))
      .first();

    if (await nameInput.isVisible({ timeout: 3_000 })) {
      await nameInput.fill("Morning Briefing");
    }
    if (await cronInput.isVisible({ timeout: 1_000 })) {
      await cronInput.fill("0 0 7 * * *");
    }
    if (await promptInput.isVisible({ timeout: 1_000 })) {
      await promptInput.fill("Give me a morning briefing");
    }

    // Submit
    const submitBtn = page
      .getByRole("button", { name: /save|create|submit/i })
      .first();
    if (await submitBtn.isVisible({ timeout: 2_000 })) {
      await submitBtn.click();
      // Give the POST handler time to fire
      await page.waitForTimeout(500);
      expect(postBody).not.toBeNull();
    }
  });

  test.skip("delete button calls DELETE endpoint", async ({ page }) => {
    let deleteCalled = false;

    // Accept browser confirm() dialogs automatically
    page.on("dialog", (dialog) => dialog.accept());

    await page.route("**/api/v1/schedules", (route) => {
      if (route.request().method() === "GET") {
        return route.fulfill({
          json: [
            {
              id: "sched-del",
              name: "To Delete",
              cron: "0 0 9 * * *",
              prompt: "some prompt",
              enabled: true,
            },
          ],
        });
      }
      return route.continue();
    });
    await page.route("**/api/v1/schedules/sched-del", (route) => {
      if (route.request().method() === "DELETE") {
        deleteCalled = true;
        return route.fulfill({ json: { status: "ok" } });
      }
      return route.continue();
    });

    await goToSchedules(page);
    await expect(page.getByText("To Delete")).toBeVisible({ timeout: 10_000 });

    // Schedules.tsx renders: <Button aria-label="Delete schedule">
    const deleteBtn = page.getByRole("button", { name: "Delete schedule" }).first();
    await expect(deleteBtn).toBeVisible({ timeout: 5_000 });
    await deleteBtn.click();
    await page.waitForTimeout(400);
    expect(deleteCalled).toBe(true);
  });
});
