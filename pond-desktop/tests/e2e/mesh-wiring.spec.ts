import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// Mesh section: trusted peers from GET /api/v1/mesh/peers, add/remove via the mocked API.

async function openMeshSection(page: import("@playwright/test").Page) {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "mesh");
  });
  await page.goto("/");
  await expect(page.locator(".screen--mesh")).toBeVisible({ timeout: 10_000 });
}

test("mesh section shows disabled state when mesh is off", async ({ page }) => {
  await openMeshSection(page);
  // Two .empty-state blocks coexist (mesh off, no peers), so filter by text for strict mode.
  await expect(
    page.locator(".empty-state").filter({ hasText: "Mesh is disabled" }),
  ).toBeVisible();
});

test("mesh section lists trusted peers from the API", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.route("**/api/v1/mesh/peers", (route) => {
    if (route.request().method() === "POST") return route.continue();
    return route.fulfill({
      json: {
        peers: [
          {
            peer_id: "a".repeat(64),
            trust_scope: "circle",
            connected: true,
            credit_balance_millisats: 5000,
          },
        ],
      },
    });
  });
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "mesh");
  });
  await page.goto("/");
  await expect(page.locator(".screen--mesh")).toBeVisible({ timeout: 10_000 });

  await expect(page.locator(".devices-grid .device-card")).toHaveCount(1);
  await expect(page.locator(".device-card__chip--online")).toContainText("connected");
});

test("add trusted peer form submits peer_id and trust_scope", async ({ page }) => {
  await openMeshSection(page);

  const addPromise = page.waitForRequest(
    (r) => r.url().includes("/api/v1/mesh/peers") && r.method() === "POST",
  );

  await page.getByRole("button", { name: /add trusted peer/i }).first().click();
  await page.locator(".sched-modal__input").fill("b".repeat(64));
  await page.getByRole("button", { name: /^add peer$/i }).click();

  const req = await addPromise;
  const body = req.postDataJSON() as { peer_id: string; trust_scope: string };
  expect(body.peer_id).toBe("b".repeat(64));
  expect(body.trust_scope).toBe("circle");
});

test("remove peer sends DELETE for the selected peer id", async ({ page }) => {
  await mockAllApiRoutes(page);
  const peerId = "c".repeat(64);
  await page.route("**/api/v1/mesh/peers", (route) => {
    if (route.request().method() === "POST") return route.continue();
    return route.fulfill({
      json: {
        peers: [
          {
            peer_id: peerId,
            trust_scope: "circle",
            connected: false,
            credit_balance_millisats: 0,
          },
        ],
      },
    });
  });
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "mesh");
  });
  await page.goto("/");
  await expect(page.locator(".devices-grid .device-card")).toHaveCount(1);

  const removePromise = page.waitForRequest(
    (r) => r.url().includes(`/api/v1/mesh/peers/${peerId}`) && r.method() === "DELETE",
  );
  await page.getByRole("button", { name: /remove/i }).click();
  await removePromise;
});
