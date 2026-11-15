import { test, expect } from "@playwright/test";
import type { Page, TestInfo } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import { contrastOf } from "./helpers/contrast";

const ready = { available: true, reason: null };

interface Pond {
  wifi: { state: "connected" | "disconnected"; ssid: string | null; address: string | null; signal: number | null };
  capabilities: { scan: typeof ready; join: typeof ready; forget: typeof ready };
  canChange: { allowed: boolean; reason: string | null };
  known: Set<string>;
  networks: Array<{ ssid: string; security: string; signal: number }>;
  joins: Array<Record<string, unknown>>;
  forgets: string[];
  settingsPuts: Array<Record<string, unknown>>;
  settings: Record<string, unknown>;
  /** While set, every read of the pond fails, as when a join moves it to another network. */
  dark: boolean;
}

function newPond(over: Partial<Pond> = {}): Pond {
  return {
    wifi: { state: "connected", ssid: "Home", address: "192.168.1.11", signal: 70 },
    capabilities: { scan: ready, join: ready, forget: ready },
    canChange: { allowed: true, reason: null },
    known: new Set(["Home", "Old flat"]),
    networks: [
      { ssid: "Home", security: "wpa2", signal: 70 },
      { ssid: "Neighbour", security: "wpa3", signal: 42 },
      { ssid: "Cafe", security: "open", signal: 20 },
      { ssid: "Office", security: "enterprise", signal: 55 },
      { ssid: "Old flat", security: "wpa2", signal: 10 },
    ],
    joins: [],
    forgets: [],
    settingsPuts: [],
    // A real pond always sends both switches.
    settings: { ble_setup_enabled: true, spoken_pairing_codes: true },
    dark: false,
    ...over,
  };
}

async function mockPond(page: Page, pond: Pond) {
  await mockAllApiRoutes(page);

  await page.route("**/api/v1/settings", (route) => {
    if (route.request().method() === "PUT") {
      const body = route.request().postDataJSON() as Record<string, unknown>;
      pond.settingsPuts.push(body);
      Object.assign(pond.settings, body);
    }
    return route.fulfill({ json: { assistant_name: "Pond", user_name: "Jerry", ...pond.settings } });
  });

  await page.route(/\/api\/v1\/connections/, async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname.replace("/api/v1/connections", "");
    if (pond.dark && !(request.method() === "POST")) return route.abort("failed");

    if (request.method() === "GET" && path === "") {
      return route.fulfill({
        json: {
          hostname: "nano",
          https_port: 4443,
          wifi: { ...pond.wifi, capabilities: pond.capabilities },
          ethernet: [{ interface: "eth0", link: "up", address: "192.168.1.20", speed_mbps: 1000 }],
          bluetooth: { enabled: pond.settings.ble_setup_enabled !== false, state: pond.settings.ble_setup_enabled === false ? "off" : "advertising", reason: null, advertising_as: pond.settings.ble_setup_enabled === false ? null : "Pond 4e81", session_open: false },
          can_change: pond.canChange,
        },
      });
    }
    if (request.method() === "GET" && path === "/wifi/networks") {
      return route.fulfill({
        json: {
          networks: pond.networks.map((n) => ({
            ...n,
            known: pond.known.has(n.ssid),
            current: pond.wifi.ssid === n.ssid,
          })),
        },
      });
    }
    if (request.method() === "POST" && path === "/wifi/join") {
      const body = request.postDataJSON() as { ssid: string; passphrase?: string };
      pond.joins.push(body);
      if (body.passphrase === "wrong-passphrase") return route.fulfill({ status: 401, json: { error: "wrong_password" } });
      pond.known.add(body.ssid);
      pond.wifi = { state: "connected", ssid: body.ssid, address: "10.0.0.7", signal: 55 };
      return route.fulfill({ json: { ssid: body.ssid, address: "10.0.0.7", https_port: 4443, hostname: "nano" } });
    }
    if (request.method() === "DELETE" && path.startsWith("/wifi/networks/")) {
      const ssid = decodeURIComponent(path.replace("/wifi/networks/", ""));
      if (!pond.known.has(ssid)) return route.fulfill({ status: 404, json: { error: "not_found" } });
      if (pond.wifi.ssid === ssid && url.searchParams.get("confirm") !== "true") {
        return route.fulfill({ status: 409, json: { error: "in_use" } });
      }
      pond.known.delete(ssid);
      pond.forgets.push(`${ssid}${url.searchParams.get("confirm") === "true" ? "?confirm=true" : ""}`);
      if (pond.wifi.ssid === ssid) pond.wifi = { state: "disconnected", ssid: null, address: null, signal: null };
      return route.fulfill({ status: 204, body: "" });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
}

async function shot(page: Page, info: TestInfo, name: string) {
  await page.screenshot({ path: info.outputPath(`${name}.png`), fullPage: true });
}

async function bootHub(page: Page, size = { width: 1024, height: 600 }, theme: "Light" | "Dark" = "Light") {
  await page.addInitScript((t) => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
    localStorage.setItem("goosehub_theme", t);
  }, theme);
  await page.setViewportSize(size);
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  await navigateTo(page, "Settings");
  await page.waitForSelector(".set", { timeout: 5_000 });
  await page.getByRole("button", { name: /^Connections$/ }).first().click();
  await expect(page.getByRole("heading", { name: "Connections", level: 1 })).toBeVisible();
}

async function bootClassic(page: Page, size = { width: 1440, height: 900 }, theme: "Light" | "Dark" = "Light") {
  await page.addInitScript((t) => localStorage.setItem("goosehub_theme", t), theme);
  await page.setViewportSize(size);
  await page.goto("/");
  await navigateTo(page, "Settings");
  await page.getByRole("button", { name: /^Connections/ }).click();
  await expect(page.getByRole("heading", { name: "Wi-Fi", level: 3 })).toBeVisible();
}

async function scanAndAssert(page: Page, label: string, scope: string) {
  const results = await new AxeBuilder({ page }).include(scope).withTags(["wcag2a", "wcag2aa"]).analyze();
  expect(results.violations, `${label}: ${JSON.stringify(results.violations, null, 2)}`).toEqual([]);
}

async function walk(page: Page, info: TestInfo, prefix: string, scope: string) {
  await expect(page.getByText("192.168.1.11")).toBeVisible();
  await expect(page.getByText("eth0")).toBeVisible();
  await expect(page.getByText("Visible to phones")).toBeVisible();
  await expect(page.getByText(/^Claimed on /)).toBeVisible();
  await shot(page, info, `${prefix}-1-open`);
  await scanAndAssert(page, `${prefix} open`, scope);

  await page.getByRole("button", { name: "Find networks" }).click();
  await expect(page.getByText("5 networks found.")).toBeVisible();
  await expect(page.getByText(/username and certificate/).first()).toBeVisible();
  await shot(page, info, `${prefix}-2-scanned`);
  await scanAndAssert(page, `${prefix} scanned`, scope);

  await page.getByRole("button", { name: "Join Neighbour" }).click();
  const field = page.getByLabel("Passphrase for Neighbour");
  await expect(field).toBeFocused();
  await expect(field).toHaveAttribute("type", "password");
  await field.fill("wrong-passphrase");
  await shot(page, info, `${prefix}-3-join-form`);
  await page.getByRole("form", { name: "Join Neighbour" }).getByRole("button", { name: "Join Neighbour" }).click();
  await expect(page.getByRole("alert")).toContainText("did not accept that passphrase");
  await shot(page, info, `${prefix}-4-wrong-passphrase`);
  await scanAndAssert(page, `${prefix} wrong passphrase`, scope);

  await field.fill("correct horse battery");
  await page.getByRole("form", { name: "Join Neighbour" }).getByRole("button", { name: "Join Neighbour" }).click();
  await expect(page.getByText(/Joined .Neighbour. at 10\.0\.0\.7/)).toBeVisible();
  await expect(page.getByText(/may lose contact with it/)).toBeVisible();
  await shot(page, info, `${prefix}-5-joined`);
}

test.describe("Connections: Hub", () => {
  test("opens from the Settings list and shows every link", async ({ page }, info) => {
    const pond = newPond();
    await mockPond(page, pond);
    await bootHub(page);
    await walk(page, info, "hub-1024x600", ".setd");
    expect(pond.joins.map((j) => j.passphrase)).toEqual(["wrong-passphrase", "correct horse battery"]);
    // No re-pair retry: a wrong passphrase must reach the pond once.
    expect(pond.joins).toHaveLength(2);
  });

  test("reads top to bottom on one tall page", async ({ page }, info) => {
    await mockPond(page, newPond());
    await bootHub(page, { width: 1024, height: 1900 });
    await page.getByRole("button", { name: "Find networks" }).click();
    await expect(page.getByText("5 networks found.")).toBeVisible();
    await shot(page, info, "hub-tall-scanned");
  });

  test("fits a 800x480 panel without sideways scrolling", async ({ page }, info) => {
    await mockPond(page, newPond());
    await bootHub(page, { width: 800, height: 480 });
    await page.getByRole("button", { name: "Find networks" }).click();
    await expect(page.getByText("5 networks found.")).toBeVisible();
    await page.getByRole("button", { name: "Join Neighbour" }).click();
    await shot(page, info, "hub-800x480-join");
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  });

  test("forgets the current network only after explaining, and then sends confirm", async ({ page }, info) => {
    const pond = newPond();
    await mockPond(page, pond);
    await bootHub(page);
    await page.getByRole("button", { name: "Forget Home" }).click();
    await expect(page.getByRole("group", { name: "Forget Home" })).toContainText("may drop off your network");
    await shot(page, info, "hub-forget-current");
    expect(pond.forgets).toEqual([]);
    await page.getByRole("button", { name: /anyway/ }).click();
    await expect(page.getByText(/Forgot .Home./)).toBeVisible();
    expect(pond.forgets).toEqual(["Home?confirm=true"]);
    await shot(page, info, "hub-forgot-current");
  });

  test("saves the Bluetooth and pairing switches as settings", async ({ page }) => {
    const pond = newPond();
    await mockPond(page, pond);
    await bootHub(page);
    await page.getByRole("switch", { name: "Set up over Bluetooth" }).click();
    await expect.poll(() => pond.settingsPuts).toContainEqual({ ble_setup_enabled: false });
    await expect(page.getByText(/Setup over Bluetooth is switched off/)).toBeVisible({ timeout: 6_000 });
    await page.getByRole("switch", { name: "Say pairing codes aloud" }).click();
    await expect.poll(() => pond.settingsPuts).toContainEqual({ spoken_pairing_codes: false });
  });

  test("disables changes, and says why, for a caller who may not make them", async ({ page }, info) => {
    const reason = "the pond's user may not change Wi-Fi (run: bash scripts/giap.sh wifi-grant)";
    await mockPond(
      page,
      newPond({
        canChange: { allowed: false, reason: "Only the phone that set up this pond, or a household member's device, can change its network" },
        capabilities: { scan: ready, join: { available: false, reason }, forget: { available: false, reason } },
      }),
    );
    await bootHub(page);
    await expect(page.getByText(/wifi-grant/)).toBeVisible();
    await expect(page.getByText(/Only the phone that set up this pond/)).toBeVisible();
    await expect(page.getByRole("button", { name: "Join a hidden network" })).toBeDisabled();
    await expect(page.getByRole("button", { name: "Forget Home" })).toBeDisabled();
    await expect(page.getByRole("button", { name: "Find networks" })).toBeEnabled();
    await shot(page, info, "hub-read-only");
    await scanAndAssert(page, "hub read only", ".setd");
  });

  test("shows progress for a slow join, keeps working, and recovers when the pond answers again", async ({ page }, info) => {
    const pond = newPond();
    await mockPond(page, pond);
    // A slow join, then a pond that goes quiet while it moves, then comes back.
    await page.route("**/api/v1/connections/wifi/join", async (route) => {
      const body = route.request().postDataJSON() as { ssid: string };
      await new Promise((r) => setTimeout(r, 1_500));
      pond.dark = true;
      pond.wifi = { state: "connected", ssid: body.ssid, address: "10.0.0.7", signal: 55 };
      setTimeout(() => (pond.dark = false), 3_000);
      return route.fulfill({ json: { ssid: body.ssid, address: "10.0.0.7", https_port: 4443, hostname: "nano" } });
    });
    await bootHub(page);
    await page.getByRole("button", { name: "Find networks" }).click();
    await page.getByRole("button", { name: "Join Neighbour" }).click();
    await page.getByLabel("Passphrase for Neighbour").fill("correct horse battery");
    await page.getByRole("form", { name: "Join Neighbour" }).getByRole("button", { name: "Join Neighbour" }).click();

    await expect(page.getByText(/This can take up to 45 seconds/)).toBeVisible();
    await shot(page, info, "hub-joining");
    // The window stays usable: nothing modal, the rest of the screen still answers.
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await page.getByRole("button", { name: "Back to Settings" }).focus();

    await expect(page.getByText(/The pond is not answering/)).toBeVisible({ timeout: 10_000 });
    await shot(page, info, "hub-pond-quiet");
    await expect(page.getByText("Back in touch with the pond.")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText("10.0.0.7").first()).toBeVisible();
    await shot(page, info, "hub-back-in-touch");
  });

  test("is operable from the keyboard", async ({ page }) => {
    await mockPond(page, newPond());
    await bootHub(page);
    const find = page.getByRole("button", { name: "Find networks" });
    await find.focus();
    await page.keyboard.press("Enter");
    await expect(page.getByText("5 networks found.")).toBeVisible();
    await page.getByRole("button", { name: "Join Neighbour" }).focus();
    await page.keyboard.press("Enter");
    await expect(page.getByLabel("Passphrase for Neighbour")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("form", { name: "Join Neighbour" })).toHaveCount(0);
  });

  test("holds up in the dark theme", async ({ page }, info) => {
    await mockPond(page, newPond());
    await bootHub(page, { width: 1024, height: 600 }, "Dark");
    await page.getByRole("button", { name: "Find networks" }).click();
    await expect(page.getByText("5 networks found.")).toBeVisible();
    await page.getByRole("button", { name: "Join Neighbour" }).click();
    await shot(page, info, "hub-dark-join");
    await scanAndAssert(page, "hub dark", ".setd");
    for (const text of ["192.168.1.11", "WPA3", "Waiting", "Claimed"]) {
      const node = page.getByText(text, { exact: false }).first();
      if (await node.count()) expect(await contrastOf(node), text).toBeGreaterThanOrEqual(4.5);
    }
  });
});

test.describe("Connections: Classic", () => {
  test("opens from the settings rail and shows every link", async ({ page }, info) => {
    const pond = newPond();
    await mockPond(page, pond);
    await bootClassic(page);
    await walk(page, info, "classic-1440x900", ".scat__panel");
    expect(pond.joins).toHaveLength(2);
  });

  test("reads top to bottom on one tall page", async ({ page }, info) => {
    await mockPond(page, newPond());
    await bootClassic(page, { width: 1440, height: 2300 });
    await page.getByRole("button", { name: "Find networks" }).click();
    await expect(page.getByText("5 networks found.")).toBeVisible();
    await shot(page, info, "classic-tall-scanned");
  });

  test("stages the Bluetooth switch for the page's Save", async ({ page }, info) => {
    const pond = newPond();
    await mockPond(page, pond);
    await bootClassic(page, { width: 1024, height: 600 });
    await page.getByRole("switch", { name: "Set up over Bluetooth" }).click();
    expect(pond.settingsPuts).toEqual([]);
    await shot(page, info, "classic-1024x600-staged");
    await page.getByRole("button", { name: /Save 1 change/ }).click();
    await expect.poll(() => pond.settingsPuts).toContainEqual({ ble_setup_enabled: false });
    await expect(page.getByText(/Setup over Bluetooth is switched off/)).toBeVisible({ timeout: 6_000 });
  });

  test("points to Connections from Privacy and Security", async ({ page }) => {
    await mockPond(page, newPond());
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/");
    await navigateTo(page, "Settings");
    await page.getByRole("button", { name: /Privacy & Security/ }).click();
    await expect(page.getByLabel("Set up over Bluetooth")).toHaveCount(0);
    await page.getByRole("button", { name: "Open Connections" }).click();
    await expect(page.getByRole("heading", { name: "Wi-Fi", level: 3 })).toBeVisible();
  });

  test("holds up in the dark theme", async ({ page }, info) => {
    await mockPond(page, newPond());
    await bootClassic(page, { width: 1440, height: 900 }, "Dark");
    await page.getByRole("button", { name: "Find networks" }).click();
    await expect(page.getByText("5 networks found.")).toBeVisible();
    await shot(page, info, "classic-dark-scanned");
    await scanAndAssert(page, "classic dark", ".scat__panel");
  });
});
