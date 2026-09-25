import { expect, test } from "@playwright/test";

/**
 * Live dashboard checks, no mocks (`tests/e2e/` mocks can't catch an API contract change).
 * Run by `scripts/live-test.sh --ui`, or: POND_LIVE_URL=http://127.0.0.1:4000 \
 *   npx playwright test --config=playwright.live.config.ts
 */

test.describe("live dashboard", () => {
  test("the server serves a real UI, not the build.rs placeholder", async ({ page }) => {
    const response = await page.goto("/");
    expect(response?.status()).toBe(200);

    // crates/pond-api/build.rs emits a stub with this attribute when the UI was never built.
    const placeholder = await page.locator("html[data-giap-placeholder]").count();
    expect(
      placeholder,
      "the server is serving the build.rs placeholder -- run `npm run build` in pond-desktop first",
    ).toBe(0);

    await expect(page).toHaveTitle(/Goose In A Pond/i);
  });

  test("the page loads without console errors or failed requests", async ({ page }) => {
    const consoleErrors: string[] = [];
    const failed: string[] = [];

    page.on("console", (m) => {
      if (m.type() === "error") consoleErrors.push(m.text());
    });
    page.on("response", (r) => {
      // Only 5xx fails: the dashboard may probe routes it isn't authorised for (401).
      if (r.status() >= 500) failed.push(`${r.status()} ${r.url()}`);
    });

    await page.goto("/");
    await page.waitForLoadState("networkidle");

    expect(failed, "server returned 5xx to the dashboard").toEqual([]);
    expect(consoleErrors, "dashboard logged console errors").toEqual([]);
  });

  test("the API the dashboard actually calls answers in the shape it expects", async ({
    request,
  }) => {
    // PondApiClient.listProfiles() reads `.profiles`; it is the UI's only profile call.
    const res = await request.get("/api/v1/profiles");
    expect(res.status()).toBe(200);
    const body = await res.json();
    expect(
      Array.isArray(body.profiles),
      `listProfiles() expects {profiles: []}, server sent ${JSON.stringify(body).slice(0, 120)}`,
    ).toBe(true);
  });

  test("health is reachable from the browser context, same-origin", async ({ page }) => {
    // In a plain browser the API base defaults to window.location.origin (defaultServerUrl()).
    await page.goto("/");
    const status = await page.evaluate(async () => {
      const r = await fetch("/api/v1/health");
      return r.status;
    });
    expect(status).toBe(200);
  });
});

// Identity has no UI beyond GET /api/v1/profiles; scripts/live_checks.py covers its API.
