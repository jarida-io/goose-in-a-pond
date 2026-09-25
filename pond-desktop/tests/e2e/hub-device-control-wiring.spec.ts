import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// Verifies the Hub device surfaces actuate through the backend device-control
// MCP tool via POST /api/v1/tools/invoke (bypassing the LLM), with optimistic UI.

async function openHubHome(page: import("@playwright/test").Page) {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");
  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator('[data-hook="home-controls"] .hcc__tile').first()).toBeVisible({ timeout: 8_000 });
}

test("device tile click fires POST /tools/invoke with the device-control tool", async ({ page }) => {
  await openHubHome(page);

  const invokePromise = page.waitForRequest(
    (r) => r.url().includes("/api/v1/tools/invoke") && r.method() === "POST",
  );

  const tile = page.locator('[data-hook="home-controls"] .hcc__tile').first();
  await tile.click();

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
  await expect(tile.locator(".hcc__value")).toContainText("On", { timeout: 5000 });
});

/**
 * There is no optimistic state left to revert, which is the point.
 *
 * The old tile wrote "On" locally and rolled back on a 500, so a backend that
 * was down still produced a tile confidently reporting a switch position. The
 * card asks instead, and a device that did not answer says so: "Not reporting",
 * and a tap that opens the control sheet rather than a switch whose direction
 * nobody knows.
 */
test("a device that cannot be read says so rather than claiming a state", async ({ page }) => {
  await mockAllApiRoutes(page);
  // Override the invoke route to fail — both the read and the write.
  await page.route("**/api/v1/tools/invoke", (route) =>
    route.fulfill({ status: 500, json: { error: "device offline" } }),
  );
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");

  const tile = page.locator('[data-hook="home-controls"] .hcc__tile').first();
  await expect(tile).toBeVisible({ timeout: 10_000 });
  await expect(tile.locator(".hcc__value")).toHaveText("Not reporting", { timeout: 5000 });
  await expect(tile).toHaveAttribute("aria-label", /not reporting/);
});
