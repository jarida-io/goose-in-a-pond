/** The Uber panel in a real browser, on the Hub's Accounts screen and under Classic's Context > Sources. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

// The API base mockAllApiRoutes pins. connectUber sends it as `origin` so the pond can send the
// browser back to where it started after Uber sign-in.
const API_BASE = "http://127.0.0.1:4000";

async function mockUber(page: Page, connected: string[]) {
  await mockAllApiRoutes(page);
  const state = { connected: [...connected], connectBody: null as unknown, deleted: [] as string[] };

  await page.route("**/api/v1/context/sources**", (r) => r.fulfill({ json: { sources: [] } }));
  await page.route("**/api/v1/profiles", (r) =>
    r.fulfill({
      json: {
        profiles: [
          { id: "p-liz", display_name: "Liz", avatar_emoji: "*" },
          { id: "p-jerry", display_name: "Jerry", avatar_emoji: "*" },
        ],
      },
    }),
  );
  await page.route("**/api/v1/uber/accounts", (r) => r.fulfill({ json: { connected: state.connected } }));
  await page.route("**/api/v1/uber/accounts/connect", (r) => {
    state.connectBody = r.request().postDataJSON();
    state.connected.push("p-jerry");
    return r.fulfill({ json: { auth_url: "https://auth.uber.com/oauth/v2/authorize?x", state: "n-1" } });
  });
  await page.route("**/api/v1/oauth/status/n-1", (r) => r.fulfill({ json: { status: "completed" } }));
  await page.route("**/api/v1/uber/accounts/p-*", (r) => {
    state.deleted.push(r.request().url().split("/").pop()!);
    state.connected = state.connected.filter((id) => !r.request().url().endsWith(id));
    return r.fulfill({ status: 204, body: "" });
  });
  // Uber's page opens in a popup, which page.route does not reach; the context's routes do.
  await page
    .context()
    .route("https://auth.uber.com/**", (r) =>
      r.fulfill({ contentType: "text/html", body: "<title>Uber sign-in stand-in</title>" }),
    );

  await page.setViewportSize({ width: 1440, height: 900 });
  return state;
}

async function openHubAccounts(page: Page, connected: string[]) {
  const state = await mockUber(page, connected);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  await navigateTo(page, "Settings");
  await page.waitForSelector(".set", { timeout: 5_000 });
  await page.getByRole("button", { name: /^Accounts$/ }).first().click();
  return state;
}

async function openClassicSources(page: Page, connected: string[]) {
  const state = await mockUber(page, connected);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "context");
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Sources" }).click();
  return state;
}

function uberPanel(page: Page) {
  return page.locator("section", { has: page.getByRole("heading", { name: "Uber" }) });
}

test.describe("Hub — Accounts: Uber", () => {
  test("shows each member's own Uber status", async ({ page }) => {
    await openHubAccounts(page, ["p-liz"]);
    const panel = uberPanel(page);
    await expect(panel.getByText("Liz")).toBeVisible({ timeout: 5_000 });
    await expect(panel.getByText("Connected", { exact: true })).toBeVisible();
    await expect(panel.getByText("Not connected")).toBeVisible();
  });

  test("connecting a member opens Uber and then shows them connected", async ({ page }) => {
    const state = await openHubAccounts(page, []);
    const panel = uberPanel(page);
    const opened = page.waitForEvent("popup");
    await panel.getByRole("button", { name: "Connect Uber" }).nth(1).click();
    const popup = await opened;
    expect(popup.url()).toContain("auth.uber.com");
    await expect(popup).toHaveTitle("Uber sign-in stand-in");
    await expect(panel.getByText("Connected", { exact: true })).toBeVisible({ timeout: 10_000 });
    expect(state.connectBody).toEqual({ profile_id: "p-jerry", origin: API_BASE });
  });

  test("disconnecting forgets only that member", async ({ page }) => {
    const state = await openHubAccounts(page, ["p-liz", "p-jerry"]);
    const panel = uberPanel(page);
    await panel.getByRole("button", { name: "Disconnect" }).first().click();
    await expect(panel.getByRole("button", { name: "Disconnect" })).toHaveCount(1, { timeout: 5_000 });
    expect(state.deleted).toEqual(["p-liz"]);
  });
});

test.describe("Classic — Context, Sources: Uber", () => {
  test("lists each member's Uber status beside the other accounts and connects one", async ({ page }) => {
    const state = await openClassicSources(page, ["p-liz"]);
    const panel = uberPanel(page);
    await expect(panel.getByText("Liz")).toBeVisible({ timeout: 5_000 });
    await expect(panel.getByText("Connected", { exact: true })).toBeVisible();

    const opened = page.waitForEvent("popup");
    await panel.getByRole("button", { name: "Connect Uber" }).click();
    await expect(await opened).toHaveTitle("Uber sign-in stand-in");
    await expect(panel.getByText("Connected", { exact: true })).toHaveCount(2, { timeout: 10_000 });
    expect(state.connectBody).toEqual({ profile_id: "p-jerry", origin: API_BASE });
  });
});
