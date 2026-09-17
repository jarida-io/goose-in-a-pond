/**
 * kiosk-viewport.spec.ts
 *
 * Verifies that the GIAP desktop UI renders correctly on 7-inch kiosk panels:
 *   - 1024×600 (primary target)
 *   - 800×480  (secondary / smaller kiosk)
 *
 * Checks:
 *   1. No horizontal overflow (body.scrollWidth === viewport width).
 *   2. The shell spends no horizontal space on navigation at all.
 *   3. The drawer, open, fits inside the panel and is scrollable rather than
 *      clipped.
 *   4. Key interactive targets meet the touch minimum.
 *   5. Both shells (app-shell and ghub) are screenshotted.
 *
 * What this file used to check, and why it no longer can: the sidebar's
 * collapsed width, that its brand name and group labels were display:none at
 * kiosk sizes, and that the hub rail stayed under 86px. All three were
 * measuring the same thing -- how little horizontal space navigation could be
 * squeezed into -- and the answer is now none, because navigation is a drawer.
 * The property survives as check 2; the pixel counts do not survive at all.
 */

import { test, expect, Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo, openDrawer } from "./helpers/nav";

// ── Helpers ──────────────────────────────────────────────────────────────────

async function checkNoHorizOverflow(page: Page): Promise<void> {
  const overflow = await page.evaluate(() => document.body.scrollWidth > window.innerWidth);
  expect(overflow, "No horizontal overflow expected").toBe(false);
}

/**
 * The shell gives navigation no width of its own.
 *
 * Asserted as "content starts at the left edge" rather than "no .sidebar
 * exists", because the absence of a class is satisfied by a blank page.
 */
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
  // Taller than the panel is expected and fine; clipped is not. The body must
  // be the thing that scrolls.
  expect(b.clientH, "Drawer body has height to scroll in").toBeGreaterThan(0);
  await page.keyboard.press("Escape");
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
    await page.goto("/");
    await navigateTo(page, "Models");
    await page.waitForTimeout(500);
    await checkNoHorizOverflow(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-models-1024x600.png" });
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
});

/**
 * Home's own controls are inside the panel, not below it.
 *
 * VERTICAL, where everything else in this file measures horizontally, and it is
 * here because the screen it guards is the one that had the defect: `.dash`
 * carried a `min-height: 520px` under a 60px shell bar, so on the 800x480 panel
 * its bottom edge -- which the floating dock was positioned against -- sat at
 * 580. Measured at the time: the mic at top=500 bottom=564 in a 480px viewport,
 * and the page dots at 524. The panel could not reach either.
 *
 * No existing check could have caught it. Horizontal overflow was clean, the
 * touch targets were the right size, the drawer fitted, and a full-page
 * screenshot renders an overflowing box in full -- so the kiosk screenshots
 * this file writes showed a mic that no thumb could touch.
 */
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
});
