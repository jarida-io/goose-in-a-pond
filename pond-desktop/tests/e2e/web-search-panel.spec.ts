/** The web search panel in a real browser: the Hub's Accounts screen and Classic's Context > Sources. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import { contrastOf } from "./helpers/contrast";

const GOOD_KEY = "BSA-e2e-good-key-0123456789";
const BAD_KEY = "BSA-e2e-bad-key";

type Local = { enabled: boolean; town: string | null };

async function mockBrave(page: Page, configured: boolean, local: Local | null = { enabled: false, town: "Nairobi" }) {
  await mockAllApiRoutes(page);
  const state = { configured, puts: [] as unknown[], deletes: 0, local, settingsPuts: [] as unknown[], settingsFail: false };

  await page.route("**/api/v1/context/sources**", (r) => r.fulfill({ json: { sources: [] } }));
  await page.route("**/api/v1/profiles", (r) => r.fulfill({ json: { profiles: [] } }));
  await page.route("**/api/v1/uber/accounts", (r) => r.fulfill({ json: { connected: [] } }));
  await page.route("**/api/v1/web-search", (r) =>
    r.fulfill({
      json: {
        provider: "brave",
        configured: state.configured,
        status: state.configured ? "ok" : "not_configured",
        ...(state.local ? { local_results: state.local } : {}),
      },
    }),
  );
  await page.route("**/api/v1/settings", (r) => {
    if (r.request().method() !== "PUT") return r.fallback();
    if (state.settingsFail) return r.fulfill({ status: 500, json: { error: "disk_full" } });
    const body = r.request().postDataJSON() as { web_search_local_results?: boolean };
    state.settingsPuts.push(body);
    if (state.local && typeof body.web_search_local_results === "boolean") {
      state.local = { ...state.local, enabled: body.web_search_local_results };
    }
    return r.fulfill({ json: { web_search_local_results: body.web_search_local_results } });
  });
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

async function openHub(page: Page, configured: boolean, scheme = "Light", local?: Local | null) {
  const state = await mockBrave(page, configured, local);
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

async function openClassic(page: Page, configured: boolean, scheme = "Light", local?: Local | null) {
  const state = await mockBrave(page, configured, local);
  await page.addInitScript((theme) => {
    localStorage.setItem("goosehub_theme", theme);
    localStorage.setItem("giap-section", "context");
  }, scheme);
  await page.goto("/");
  await page.getByRole("button", { name: "Sources" }).click();
  return state;
}

function panel(page: Page) {
  return page.locator("section", { has: page.getByRole("heading", { name: "Web search" }) });
}

type Open = (page: Page, configured: boolean, scheme?: string, local?: Local | null) => ReturnType<typeof mockBrave>;

const surfaces: ReadonlyArray<readonly [string, Open]> = [
  ["Hub, Accounts", openHub],
  ["Classic, Context > Sources", openClassic],
];

const SENDS_NAIROBI =
  "Sends your town (Nairobi) to Brave with each search, so it can find places near you. Your exact location is never sent.";

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

    test("nearby places starts off, and says what would leave the house before it is turned on", async ({ page }) => {
      await open(page, true);
      const p = panel(page);
      const places = p.getByRole("switch", { name: "Nearby places" });
      await expect(places).toBeVisible({ timeout: 5_000 });
      await expect(places).toHaveAttribute("aria-checked", "false");
      await expect(p.getByText(SENDS_NAIROBI)).toBeVisible();
      await expect(p.getByText(/Each web search sends Nairobi/)).toHaveCount(0);
    });

    test("turning nearby places on saves that one setting and names the town in the privacy line", async ({ page }) => {
      const state = await open(page, true);
      const p = panel(page);
      const places = p.getByRole("switch", { name: "Nearby places" });
      await expect(places).toHaveAttribute("aria-checked", "false", { timeout: 5_000 });

      await places.click();
      await expect(places).toHaveAttribute("aria-checked", "true");
      await expect(p.getByText(/Each web search sends Nairobi to Brave, so the results can include places near you\./)).toBeVisible();
      await expect.poll(() => state.settingsPuts).toEqual([{ web_search_local_results: true }]);

      await places.click();
      await expect(places).toHaveAttribute("aria-checked", "false");
      await expect(p.getByText(/Each web search sends Nairobi/)).toHaveCount(0);
      await expect.poll(() => state.settingsPuts).toEqual([
        { web_search_local_results: true },
        { web_search_local_results: false },
      ]);
    });

    test("nearby places is still on after the screen is opened again", async ({ page }) => {
      await open(page, true, "Light", { enabled: true, town: "Nairobi" });
      await expect(panel(page).getByRole("switch", { name: "Nearby places" })).toHaveAttribute("aria-checked", "true", {
        timeout: 5_000,
      });
    });

    test("a save the pond refuses puts the switch back and says so", async ({ page }) => {
      const state = await open(page, true);
      state.settingsFail = true;
      const p = panel(page);
      const places = p.getByRole("switch", { name: "Nearby places" });
      await expect(places).toBeVisible({ timeout: 5_000 });

      await places.click();
      await expect(p.getByRole("alert")).toHaveText("Could not save this setting. Try again in a moment.");
      await expect(places).toHaveAttribute("aria-checked", "false");
      await expect(p.getByText(/Each web search sends Nairobi/)).toHaveCount(0);
    });

    test("with no town it cannot be turned on, and says to set the location first", async ({ page }) => {
      const state = await open(page, true, "Light", { enabled: false, town: null });
      const p = panel(page);
      const places = p.getByRole("switch", { name: "Nearby places" });
      await expect(places).toBeVisible({ timeout: 5_000 });
      await expect(p.getByText(/Needs your location first/)).toBeVisible();

      // Forced: Playwright treats aria-disabled as unclickable, and a person's click still lands.
      await places.click({ force: true });
      await expect(places).toHaveAttribute("aria-checked", "false");
      expect(state.settingsPuts).toEqual([]);
    });

    test("the switch can be reached and turned with the keyboard", async ({ page }) => {
      const state = await open(page, true);
      const places = panel(page).getByRole("switch", { name: "Nearby places" });
      await expect(places).toBeVisible({ timeout: 5_000 });
      await places.focus();
      await expect(places).toBeFocused();
      await page.keyboard.press("Space");
      await expect(places).toHaveAttribute("aria-checked", "true");
      await expect.poll(() => state.settingsPuts).toEqual([{ web_search_local_results: true }]);
    });

    test("the switch is at least 44px square to hit", async ({ page }) => {
      await open(page, true);
      const box = await panel(page).getByRole("switch", { name: "Nearby places" }).boundingBox();
      expect(box?.width).toBeGreaterThanOrEqual(44);
      expect(box?.height).toBeGreaterThanOrEqual(44);
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

      await panel(page).getByRole("switch", { name: "Nearby places" }).click();
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

/** WCAG 1.4.11: the switch's own edge, over the card it sits on, needs 3:1 in both states and themes. */
for (const scheme of ["Light", "Dark"]) {
  for (const [surface, open] of surfaces) {
    test(`the nearby places switch keeps a 3:1 edge, off and on, ${surface}, ${scheme}`, async ({ page }) => {
      await open(page, true, scheme);
      const sw = panel(page).getByRole("switch", { name: "Nearby places" });
      await expect(sw).toBeVisible({ timeout: 5_000 });
      const edge = () =>
        sw.locator(".websearch-switch__track").evaluate((track) => {
          const rgb = (css: string) => (css.match(/[\d.]+/g) ?? []).map(Number);
          const lum = ([r, g, b]: number[]) => {
            const c = (v: number) => (v / 255 <= 0.03928 ? v / 255 / 12.92 : ((v / 255 + 0.055) / 1.055) ** 2.4);
            return 0.2126 * c(r) + 0.7152 * c(g) + 0.0722 * c(b);
          };
          // The first opaque ancestor background is what the track's border meets.
          let behind = [255, 255, 255];
          for (let e: Element | null = track.parentElement; e; e = e.parentElement) {
            const bg = rgb(getComputedStyle(e).backgroundColor);
            if (bg.length >= 3 && (bg[3] ?? 1) === 1) {
              behind = bg;
              break;
            }
          }
          const [r, g, b, a = 1] = rgb(getComputedStyle(track).borderTopColor);
          // A translucent edge is seen over the card, so it is measured over it.
          const border = [r, g, b].map((v, i) => v * a + behind[i] * (1 - a));
          const [hi, lo] = [lum(border), lum(behind)].sort((a, b) => b - a);
          return (hi + 0.05) / (lo + 0.05);
        });
      expect(await edge()).toBeGreaterThanOrEqual(3);
      await sw.click();
      await expect(sw).toHaveAttribute("aria-checked", "true");
      expect(await edge()).toBeGreaterThanOrEqual(3);
    });
  }
}
