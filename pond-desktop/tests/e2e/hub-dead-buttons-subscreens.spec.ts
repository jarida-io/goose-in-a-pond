import { test } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

const SUBSCREENS: Array<{ row: RegExp; label: string }> = [
  { row: /^Models$/,            label: "Models" },
  { row: /^Prompts$/,           label: "Prompts" },
  { row: /^Voice$/,             label: "Voice" },
  { row: /^Memory$/,            label: "Memory" },
  { row: /^Extensions/,         label: "Extensions" },
  { row: /^Logs$/,              label: "Logs" },
  { row: /^Privacy$/,           label: "Privacy" },
  { row: /^Rooms/,              label: "Rooms" },
  { row: /^Cameras$/,           label: "Cameras" },
  { row: /^Appearance$/,        label: "Appearance" },
  { row: /^Account$/,           label: "Account" },
];

test.setTimeout(120_000);
test("Settings sub-screen dead-button audit", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  // Settings is a drawer row now, not a rail icon. The sub-rows themselves are
  // still on the settings screen, so only the way in changes.
  await navigateTo(page, "Settings");
  await page.waitForSelector(".set", { timeout: 5_000 });

  const inventory: Array<{ view: string; text: string; hasOnClick: boolean; cls: string }> = [];

  for (const { row, label } of SUBSCREENS) {
    await page.getByRole("button", { name: row }).first().click();
    await page.waitForTimeout(250);

    const buttons = await page.evaluate(() => {
      const out: Array<{ text: string; hasOnClick: boolean; cls: string }> = [];
      const main = document.querySelector(".ghub__main");
      if (!main) return out;
      main.querySelectorAll("button").forEach((b) => {
        const text = (b.textContent || "").trim().slice(0, 50) || "(no text)";
        const propsKey = Object.keys(b).find((k) => k.startsWith("__reactProps$"));
        const props = propsKey ? (b as unknown as Record<string, { onClick?: unknown }>)[propsKey] : undefined;
        const hasOnClick = !!(props && props.onClick);
        out.push({ text, hasOnClick, cls: b.className.slice(0, 40) });
      });
      return out;
    });

    for (const b of buttons) inventory.push({ view: `Settings/${label}`, ...b });

    // Back to Settings root, through the drawer for the same reason.
    await navigateTo(page, "Settings");
    await page.waitForSelector(".set", { timeout: 4_000 });
  }

  const dead = inventory.filter((b) => !b.hasOnClick);
  // eslint-disable-next-line no-console
  console.log("SUB_INVENTORY=" + inventory.length);
  console.log("SUB_DEAD=" + JSON.stringify(dead, null, 0));
});
