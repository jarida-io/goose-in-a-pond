/** The web search panel in a real browser: the Hub's Accounts screen and Classic's Context > Sources. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import { contrastOf } from "./helpers/contrast";

const GOOD_KEY = "BSA-e2e-good-key-0123456789";
const BAD_KEY = "BSA-e2e-bad-key";

async function mockBrave(page: Page, configured: boolean) {
  await mockAllApiRoutes(page);
  const state = { configured, puts: [] as unknown[], deletes: 0 };

  await page.route("**/api/v1/context/sources**", (r) => r.fulfill({ json: { sources: [] } }));
  await page.route("**/api/v1/profiles", (r) => r.fulfill({ json: { profiles: [] } }));
  await page.route("**/api/v1/uber/accounts", (r) => r.fulfill({ json: { connected: [] } }));
  await page.route("**/api/v1/web-search", (r) =>
    r.fulfill({
      json: {
        provider: "brave",
        configured: state.configured,
        status: state.configured ? "ok" : "not_configured",
      },
    }),
  );
  await page.route("**/api/v1/web-search/key", (r) => {
    const method = r.request().method();
    if (method === "DELETE") {
      state.deletes += 1;
      state.configured = false;
      return r.fulfill({ status: 204, body: "" });
    }
    const body = r.request().postDataJSON() as { key: string };
    state.puts.push(body);
    if (body.key === BAD_KEY) {
      return r.fulfill({ status: 422, json: { error: "Brave did not accept this key." } });
    }
    state.configured = true;
    return r.fulfill({ json: { configured: true, status: "ok" } });
  });

  await page.setViewportSize({ width: 1440, height: 900 });
  return state;
}

async function openHub(page: Page, configured: boolean, scheme = "Light") {
  const state = await mockBrave(page, configured);
  await page.addInitScript((theme) => {
    localStorage.setItem("goosehub_theme", theme);
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  }, scheme);
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  await navigateTo(page, "Settings");
  await page.waitForSelector(".set", { timeout: 5_000 });
  await page.getByRole("button", { name: /^Accounts$/ }).first().click();
  return state;
}

async function openClassic(page: Page, configured: boolean) {
  const state = await mockBrave(page, configured);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "context");
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Sources" }).click();
  return state;
}

function panel(page: Page) {
  return page.locator("section", { has: page.getByRole("heading", { name: "Web search" }) });
}

const surfaces = [
  ["Hub, Accounts", openHub],
  ["Classic, Context > Sources", openClassic],
] as const;

for (const [name, open] of surfaces) {
  test.describe(name, () => {
    test("starts Off, says where to get a key and what leaves the pond", async ({ page }) => {
      await open(page, false);
      const p = panel(page);
      await expect(p.getByText("Off", { exact: true })).toBeVisible({ timeout: 5_000 });
      await expect(p.getByText(/your question is sent to Brave/)).toBeVisible();
      await expect(p.getByRole("link", { name: "api-dashboard.search.brave.com" })).toHaveAttribute(
        "href",
        "https://api-dashboard.search.brave.com",
      );
      await expect(p.getByRole("button", { name: /remove/i })).toHaveCount(0);
      await expect(p.getByLabel("Brave Search key")).toHaveAttribute("type", "password");
    });

    test("a refused key is refused inline, then a good key turns search On and clears the field", async ({ page }) => {
      const state = await open(page, false);
      const p = panel(page);
      const field = p.getByLabel("Brave Search key");
      await expect(p.getByText("Off", { exact: true })).toBeVisible({ timeout: 5_000 });

      await field.fill(BAD_KEY);
      await p.getByRole("button", { name: "Save" }).click();
      await expect(p.getByRole("alert")).toHaveText("Brave did not accept this key.");
      await expect(p.getByText("Off", { exact: true })).toBeVisible();

      await field.fill(GOOD_KEY);
      await p.getByRole("button", { name: "Save" }).click();
      await expect(p.getByText("On, searching with Brave")).toBeVisible({ timeout: 5_000 });
      await expect(field).toHaveValue("");
      expect(state.puts).toEqual([{ key: BAD_KEY }, { key: GOOD_KEY }]);
      await expect(page.locator("body")).not.toContainText(GOOD_KEY);
    });

    test("removing the key asks first, then turns search Off", async ({ page }) => {
      const state = await open(page, true);
      const p = panel(page);
      await expect(p.getByText("On, searching with Brave")).toBeVisible({ timeout: 5_000 });

      await p.getByRole("button", { name: "Remove the Brave key" }).click();
      await p.getByRole("button", { name: "Keep" }).click();
      expect(state.deletes).toBe(0);

      await p.getByRole("button", { name: "Remove the Brave key" }).click();
      await p.getByRole("button", { name: "Remove", exact: true }).click();
      await expect(p.getByText("Off", { exact: true })).toBeVisible({ timeout: 5_000 });
      expect(state.deletes).toBe(1);
      await expect(p.getByRole("button", { name: /remove/i })).toHaveCount(0);
    });

    test("has no WCAG 2 A/AA violations, set up or not", async ({ page }) => {
      await open(page, true);
      await expect(panel(page).getByText("On, searching with Brave")).toBeVisible({ timeout: 5_000 });
      const scan = () =>
        new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa"])
          .include('section[aria-labelledby="web-search-title"]')
          .analyze();

      expect((await scan()).violations).toEqual([]);

      await panel(page).getByRole("button", { name: "Remove the Brave key" }).click();
      expect((await scan()).violations).toEqual([]);
    });
  });
}

for (const scheme of ["Light", "Dark"]) {
  test(`the help link reads at 4.5:1 on its tinted hint, ${scheme}`, async ({ page }) => {
    await openHub(page, false, scheme);
    const link = panel(page).getByRole("link", { name: "api-dashboard.search.brave.com" });
    await expect(link).toBeVisible({ timeout: 5_000 });
    // Measured by hand: axe cannot resolve a translucent background, so it never flags this.
    expect(await contrastOf(link)).toBeGreaterThanOrEqual(4.5);
  });
}
