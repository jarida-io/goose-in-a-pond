import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// Hub device tiles actuate via POST /api/v1/tools/invoke (no LLM), with optimistic UI.

async function openHubHome(page: import("@playwright/test").Page) {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");
  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".dtile").first()).toBeVisible({ timeout: 8_000 });
}

test("device tile click fires POST /tools/invoke with the device-control tool", async ({ page }) => {
  await openHubHome(page);

  const invokePromise = page.waitForRequest(
    (r) => r.url().includes("/api/v1/tools/invoke") && r.method() === "POST",
  );

  const tile = page.locator(".dtile").first();
  await tile.click({ position: { x: 60, y: 80 } });

  const req = await invokePromise;
  const body = req.postDataJSON() as {
    server: string;
    tool: string;
    args: Record<string, unknown>;
  };
  expect(body.server).toBe("giap-device-control");
  expect(body.tool).toBe("set_device_state");
  expect(body.args).toHaveProperty("device_id");
  // First tile is the Driveway Light (starts Off) → turning on sends power:true.
  expect(body.args.power).toBe(true);

  // Optimistic UI: the mocked success keeps it On (no revert).
  await expect(tile.locator(".dtile__status")).toContainText("On", { timeout: 5000 });
});

test("optimistic state reverts when the backend call fails", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.route("**/api/v1/tools/invoke", (route) =>
    route.fulfill({ status: 500, json: { error: "device offline" } }),
  );
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");
  await expect(page.locator(".dtile").first()).toBeVisible({ timeout: 10_000 });

  const tile = page.locator(".dtile").first();
  await expect(tile.locator(".dtile__status")).toHaveText("Off", { timeout: 5000 });
  await tile.click({ position: { x: 60, y: 80 } });

  await expect(tile.locator(".dtile__status")).toHaveText("Off", { timeout: 5000 });
});
