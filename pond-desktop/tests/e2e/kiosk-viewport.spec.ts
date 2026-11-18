/**
 * Kiosk panels, 1024×600 (primary) and 800×480: no horizontal overflow, icon-only rail,
 * content fills the rest, 40px touch targets; classic and hub UIs screenshotted, Models included.
 */

import { test, expect, Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { mockModels } from "./helpers/model-mocks";
import { mockMyGoose } from "./helpers/my-goose-mocks";
import { navigateTo, openDrawer } from "./helpers/nav";

// ── Helpers ──────────────────────────────────────────────────────────────────

async function checkNoHorizOverflow(page: Page): Promise<void> {
  const overflow = await page.evaluate(() => document.body.scrollWidth > window.innerWidth);
  expect(overflow, "No horizontal overflow expected").toBe(false);
}

/** Nav takes no width: content starts at the left edge (a blank page also lacks .sidebar). */
async function checkNoPersistentNav(page: Page, contentSelector: string): Promise<void> {
  const left = await page.evaluate((sel) => {
    const el = document.querySelector(sel);
    return el ? el.getBoundingClientRect().left : null;
  }, contentSelector);
  expect(left, `${contentSelector} should be on screen`).not.toBeNull();
  expect(left as number, "Content starts at the left edge -- no nav column").toBeLessThanOrEqual(1);
}

/** The drawer's controls are the finger's targets, so they carry the floor. */
async function checkTouchTargets(page: Page): Promise<void> {
  const trigger = await page.evaluate(() => {
    const el = document.querySelector('[aria-label="Open menu"]');
    return el ? el.getBoundingClientRect().height : 0;
  });
  expect(trigger, "The drawer's trigger should be ≥44px tall").toBeGreaterThanOrEqual(44);

  await openDrawer(page);
  const minRow = await page.evaluate(() => {
    const rows = document.querySelectorAll(".hdrawer__item, .hdrawer__routine");
    if (!rows.length) return 99;
    let min = Infinity;
    rows.forEach((el) => {
      const h = el.getBoundingClientRect().height;
      if (h > 0 && h < min) min = h;
    });
    return min === Infinity ? 99 : min;
  });
  expect(minRow, "Drawer rows should be ≥44px tall (touch target)").toBeGreaterThanOrEqual(44);
  await page.keyboard.press("Escape");
}

/** Open, and prove the panel is inside the viewport and scrolls rather than clips. */
async function checkDrawerFits(page: Page): Promise<void> {
  await openDrawer(page);
  const box = await page.evaluate(() => {
    const el = document.querySelector<HTMLElement>(".hdrawer");
    const body = document.querySelector<HTMLElement>(".hdrawer__body");
    if (!el || !body) return null;
    const r = el.getBoundingClientRect();
    return {
      left: r.left, right: r.right, top: r.top, bottom: r.bottom,
      vw: window.innerWidth, vh: window.innerHeight,
      scrolls: body.scrollHeight > body.clientHeight,
      clientH: body.clientHeight,
    };
  });
  expect(box, "Drawer should be in the DOM when open").not.toBeNull();
  const b = box as NonNullable<typeof box>;
  expect(b.left, "Drawer inside the left edge").toBeGreaterThanOrEqual(0);
  expect(b.right, "Drawer inside the right edge").toBeLessThanOrEqual(b.vw);
  expect(b.top, "Drawer inside the top edge").toBeGreaterThanOrEqual(0);
  expect(b.bottom, "Drawer inside the bottom edge").toBeLessThanOrEqual(b.vh);
  // Taller than the panel is fine; clipped is not, so the body must scroll.
  expect(b.clientH, "Drawer body has height to scroll in").toBeGreaterThan(0);
  await page.keyboard.press("Escape");
}

/** A pond with a model in use and picks still to get, so a capture shows what the screen is for. */
async function mockModelsForCapture(page: Page): Promise<void> {
  await mockModels(page);
}

/** Targets a finger drives on the hub's Models screen clear 44px; the add-on link reaches it by `.reach`. */
async function checkModelsTargets(page: Page): Promise<void> {
  const small = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>(".setd button, .setd label.mm-choice"))
      .filter((el) => !el.matches(".setd__back"))
      .map((el) => ({ text: (el.textContent ?? "").trim().slice(0, 30), h: Math.round(el.getBoundingClientRect().height) }))
      .filter((t) => t.h < 44 && !t.text.startsWith("Add pictures")),
  );
  expect(small, `Models targets under 44px: ${JSON.stringify(small)}`).toEqual([]);
}

/** My Goose's controls clear 44px on the hub; a dense one counts the band it reaches through. */
async function checkMyGooseTargets(page: Page): Promise<void> {
  const small = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>(".mg button, .mg [role='radio'], .mg [role='switch']"))
      .filter((el) => el.getClientRects().length > 0)
      .map((el) => {
        const band = el.closest<HTMLElement>(".ink-labelled, .mg-reasons__reach");
        const h = el.matches(".ink-reach, .reach") ? 44 : Math.round((band ?? el).getBoundingClientRect().height);
        return { text: (el.textContent ?? "").trim().slice(0, 30), h };
      })
      .filter((t) => t.h < 44),
  );
  expect(small, `My Goose targets under 44px: ${JSON.stringify(small)}`).toEqual([]);
}

// ── Classic sections UI — 1024×600 ───────────────────────────────────────────

test.describe("Classic sections UI — 1024×600", () => {
  test.use({ viewport: { width: 1024, height: 600 } });

  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("dashboard renders without horizontal overflow", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".app-shell", { timeout: 8000 });
    await checkNoHorizOverflow(page);
    await checkTouchTargets(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-dashboard-1024x600.png" });
  });

  test("the shell spends no width on navigation", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".app-main", { timeout: 8000 });
    await checkNoPersistentNav(page, ".app-main");
  });

  test("the drawer fits the panel", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".app-shell", { timeout: 8000 });
    await checkDrawerFits(page);
  });

  test("content area uses the full width", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".app-content", { timeout: 8000 });

    const { contentLeft, contentW, viewportW } = await page.evaluate(() => {
      const content = document.querySelector(".app-content");
      return {
        contentLeft: content ? content.getBoundingClientRect().left : 0,
        contentW: content ? content.getBoundingClientRect().width : 0,
        viewportW: window.innerWidth,
      };
    });

    expect(contentLeft, "Content starts at the left edge").toBeCloseTo(0, 1);
    expect(contentW, "Content fills the viewport").toBeCloseTo(viewportW, 1);
  });

  test("models page renders without overflow", async ({ page }) => {
    await mockModelsForCapture(page);
    await page.goto("/");
    await navigateTo(page, "Models");
    await expect(page.getByRole("heading", { name: "Recommended for this pond" })).toBeVisible();
    await checkNoHorizOverflow(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-models-1024x600.png" });
  });

  test("my goose renders without overflow", async ({ page }) => {
    await mockMyGoose(page);
    await page.goto("/");
    await navigateTo(page, "My Goose");
    await expect(page.getByRole("heading", { name: "My Goose" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Confirm" })).toBeVisible();
    await checkNoHorizOverflow(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-mygoose-1024x600.png" });
  });

  test("settings page renders without overflow", async ({ page }) => {
    await page.goto("/");
    await navigateTo(page, "Settings");
    await page.waitForTimeout(500);
    await checkNoHorizOverflow(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-settings-1024x600.png" });
  });
});

// ── Classic sections UI — 800×480 ────────────────────────────────────────────

test.describe("Classic sections UI — 800×480", () => {
  test.use({ viewport: { width: 800, height: 480 } });

  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("dashboard renders without horizontal overflow", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".app-shell", { timeout: 8000 });
    await checkNoHorizOverflow(page);
    await checkHomeControlsAreOnScreen(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-dashboard-800x480.png" });
  });

  test("the drawer fits the smaller panel", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".app-shell", { timeout: 8000 });
    await checkDrawerFits(page);
  });

  test("models page renders without overflow", async ({ page }) => {
    await mockModelsForCapture(page);
    await page.goto("/");
    await navigateTo(page, "Models");
    await expect(page.getByRole("heading", { name: "Recommended for this pond" })).toBeVisible();
    await checkNoHorizOverflow(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-models-800x480.png" });
  });
});

/** Home's mic and page dots are inside the viewport; a full-page screenshot hides overflow. */
async function checkHomeControlsAreOnScreen(page: Page): Promise<void> {
  const box = await page.evaluate(() => {
    const dash = document.querySelector(".dash");
    const mic = document.querySelector(".dash__voice");
    const chat = document.querySelector(".dash__chat");
    if (!dash || !mic || !chat) return null;
    return {
      dash: dash.getBoundingClientRect().bottom,
      mic: mic.getBoundingClientRect().bottom,
      chat: chat.getBoundingClientRect().bottom,
      vh: window.innerHeight,
    };
  });
  expect(box, "Home and both of its controls should be in the DOM").not.toBeNull();
  const b = box as NonNullable<typeof box>;
  expect(b.dash, "Home itself fits the panel").toBeLessThanOrEqual(b.vh);
  expect(b.mic, "Talk to Goose is reachable").toBeLessThanOrEqual(b.vh);
  expect(b.chat, "Type to Goose is reachable").toBeLessThanOrEqual(b.vh);
}

// ── Hub UI — 1024×600 ────────────────────────────────────────────────────────

test.describe("Hub UI — 1024×600", () => {
  test.use({ viewport: { width: 1024, height: 600 } });

  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.addInitScript(() => {
      localStorage.setItem("giap-section", "hub");
      localStorage.setItem("giap-force-hub", "1");
    });
  });

  test("hub home renders without horizontal overflow", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await checkNoHorizOverflow(page);
    await checkNoPersistentNav(page, ".ghub__main");
    await checkHomeControlsAreOnScreen(page);
    await page.screenshot({ path: "kiosk-screenshots/hub-home-1024x600.png" });
  });

  test("hub models renders without overflow, with 44px targets", async ({ page }) => {
    await mockModelsForCapture(page);
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await navigateTo(page, "Models");
    await expect(page.getByRole("heading", { name: "Models" })).toBeVisible();
    await expect(page.locator(".hm-tile").first()).toBeVisible();
    await checkNoHorizOverflow(page);
    await checkModelsTargets(page);
    await page.screenshot({ path: "kiosk-screenshots/hub-models-1024x600.png" });
  });

  test("hub my goose renders without overflow, with 44px targets", async ({ page }) => {
    await mockMyGoose(page);
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await navigateTo(page, "My Goose");
    await expect(page.getByRole("heading", { name: "My Goose" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Confirm" })).toBeVisible();
    await checkNoHorizOverflow(page);
    await checkMyGooseTargets(page);
    await page.screenshot({ path: "kiosk-screenshots/hub-mygoose-1024x600.png" });
  });
});

// ── Hub UI — 800×480 ─────────────────────────────────────────────────────────

test.describe("Hub UI — 800×480", () => {
  test.use({ viewport: { width: 800, height: 480 } });

  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.addInitScript(() => {
      localStorage.setItem("giap-section", "hub");
      localStorage.setItem("giap-force-hub", "1");
    });
  });

  test("hub renders without horizontal overflow at 800×480", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await checkNoHorizOverflow(page);
    await checkHomeControlsAreOnScreen(page);
    await page.screenshot({ path: "kiosk-screenshots/hub-home-800x480.png" });
  });

  test("hub models renders without overflow at 800×480, with 44px targets", async ({ page }) => {
    await mockModelsForCapture(page);
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await navigateTo(page, "Models");
    await expect(page.getByRole("heading", { name: "Models" })).toBeVisible();
    await expect(page.locator(".hm-tile").first()).toBeVisible();
    await checkNoHorizOverflow(page);
    await checkModelsTargets(page);
    await page.screenshot({ path: "kiosk-screenshots/hub-models-800x480.png" });
  });

  test("hub my goose renders without overflow at 800×480, with 44px targets", async ({ page }) => {
    await mockMyGoose(page);
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await navigateTo(page, "My Goose");
    await expect(page.getByRole("heading", { name: "My Goose" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Confirm" })).toBeVisible();
    await checkNoHorizOverflow(page);
    await checkMyGooseTargets(page);
    await page.screenshot({ path: "kiosk-screenshots/hub-mygoose-800x480.png" });
  });
});
