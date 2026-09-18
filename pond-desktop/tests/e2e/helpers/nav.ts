// ────────────────────────────────────────────────────────────
// Reaching a destination, now that the sidebar is a drawer.
//
// The classic sidebar and the hub's icon rail both put every destination on
// screen at all times, so a spec could click one with a single locator. The
// drawer does not: a destination takes an open, sometimes a group expand, then
// the click. That is three steps in every spec that navigates, which is what
// this file exists to stop.
// ────────────────────────────────────────────────────────────

import { expect, type Page, type Locator } from "@playwright/test";

/** The twelve that live behind "Manage" rather than in the top list. */
const MANAGE_ITEMS = new Set([
  "Devices", "Mesh", "Pairing", "Schedules", "Context", "Skills",
  "Recipes", "Logs", "Models", "Prompts", "Extensions", "Faces",
]);

/**
 * Open the drawer and return its "Go to" nav.
 *
 * Scoped rather than global on purpose: "Home" is both a destination and a
 * room name, so an unscoped getByRole matches two buttons and trips strict
 * mode.
 */
export async function openDrawer(page: Page): Promise<Locator> {
  await page.locator('[aria-label="Open menu"]').click();
  await expect(page.locator('[role="dialog"][aria-label="Menu"]')).toBeVisible({
    timeout: 10_000,
  });
  return page.getByRole("navigation", { name: "Go to" });
}

/** Open the drawer, expand the group holding `label` if it has one, and click it. */
export async function navigateTo(page: Page, label: string): Promise<void> {
  const goTo = await openDrawer(page);

  if (MANAGE_ITEMS.has(label)) {
    const manage = goTo.getByRole("button", { name: "Manage" });
    if ((await manage.getAttribute("aria-expanded")) !== "true") {
      await manage.click();
    }
  }
  if (label === "Chat" || label === "Voice") {
    const pond = goTo.getByRole("button", { name: "Pond" });
    if ((await pond.getAttribute("aria-expanded")) !== "true") {
      await pond.click();
    }
  }

  await goTo.getByRole("button", { name: label, exact: true }).click();
  await expect(page.locator('[role="dialog"][aria-label="Menu"]')).toHaveCount(0);
}
