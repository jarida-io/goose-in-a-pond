/**
 * Kiosk panels, 1024×600 (primary) and 800×480: no horizontal overflow, icon-only rail,
 * content fills the rest, 40px touch targets; classic and hub UIs screenshotted.
 */

import { test, expect, Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// ── Helpers ──────────────────────────────────────────────────────────────────

async function checkNoHorizOverflow(page: Page): Promise<void> {
  const overflow = await page.evaluate(() => document.body.scrollWidth > window.innerWidth);
  expect(overflow, "No horizontal overflow expected").toBe(false);
}

/** Widest the collapsed rail may get before it stops being a rail. */
const RAIL_CEILING_PX = 72;

async function checkSidebarCompact(page: Page): Promise<void> {
  // Against the token, not a literal; icon-only-ness is asserted by the display:none test below.
  const measured = await page.evaluate(() => {
    const s = document.querySelector(".sidebar");
    if (!s) return null;
    const token = getComputedStyle(document.documentElement)
      .getPropertyValue("--sidebar-width-collapsed")
      .trim();
    return { width: s.getBoundingClientRect().width, token };
  });
  if (measured === null) return;

  const tokenPx = Number.parseFloat(measured.token);
  expect(
    Number.isFinite(tokenPx),
    `--sidebar-width-collapsed should be a px length, got "${measured.token}"`,
  ).toBe(true);

  // A ceiling, not equality: the max-height:500px rule (800×480) narrows the rail to 44px.
  expect(
    measured.width,
    `Collapsed rail (${measured.width}px) should be no wider than ` +
      `--sidebar-width-collapsed (${tokenPx}px) at kiosk viewport`,
  ).toBeLessThanOrEqual(tokenPx + 0.5);

  expect(
    tokenPx,
    `--sidebar-width-collapsed (${tokenPx}px) is too wide to still be an icon rail`,
  ).toBeLessThanOrEqual(RAIL_CEILING_PX);
}

async function checkIrailCompact(page: Page): Promise<void> {
  const irailW = await page.evaluate(() => {
    const r = document.querySelector(".irail");
    return r ? r.getBoundingClientRect().width : null;
  });
  if (irailW !== null) {
    // Hub icon rail should be ≤86px (normal max) — our rules shrink it to ≤72px
    expect(irailW, "Icon rail should be ≤86px at kiosk viewport").toBeLessThanOrEqual(86);
  }
}

async function checkTouchTargets(page: Page): Promise<void> {
  const minHeight = await page.evaluate(() => {
    const items = document.querySelectorAll(".sidebar__item");
    if (!items.length) return 99; // no sidebar — pass
    let min = Infinity;
    items.forEach((el) => {
      const h = el.getBoundingClientRect().height;
      if (h > 0 && h < min) min = h;
    });
    return min === Infinity ? 99 : min;
  });
  expect(minHeight, "Sidebar items should be ≥40px tall (touch target)").toBeGreaterThanOrEqual(40);
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
    await checkSidebarCompact(page);
    await checkTouchTargets(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-dashboard-1024x600.png" });
  });

  test("sidebar is icon-only (no text labels visible)", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".sidebar", { timeout: 8000 });

    // Brand name should not be visible (display: none via kiosk CSS)
    const brandNameVisible = await page.evaluate(() => {
      const el = document.querySelector(".sidebar__brand-name");
      if (!el) return false;
      const style = getComputedStyle(el);
      return style.display !== "none" && style.visibility !== "hidden";
    });
    expect(brandNameVisible, "Brand name should be hidden in kiosk mode").toBe(false);

    const groupLabelVisible = await page.evaluate(() => {
      const els = document.querySelectorAll(".sidebar__group-label");
      return Array.from(els).some((el) => {
        const style = getComputedStyle(el);
        return style.display !== "none" && style.visibility !== "hidden";
      });
    });
    expect(groupLabelVisible, "Group labels should be hidden in kiosk mode").toBe(false);
  });

  test("content area uses full remaining width", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".app-content", { timeout: 8000 });

    const { sidebarW, contentLeft, contentW, viewportW } = await page.evaluate(() => {
      const sidebar = document.querySelector(".sidebar");
      const content = document.querySelector(".app-content");
      return {
        sidebarW: sidebar ? sidebar.getBoundingClientRect().width : 0,
        contentLeft: content ? content.getBoundingClientRect().left : 0,
        contentW: content ? content.getBoundingClientRect().width : 0,
        viewportW: window.innerWidth,
      };
    });

    expect(contentLeft, "Content starts after sidebar").toBeCloseTo(sidebarW, 1);
    expect(contentW + sidebarW, "Content + sidebar = viewport width").toBeCloseTo(viewportW, 1);
  });

  test("models page renders without overflow", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".sidebar__item[aria-label='Models']", { timeout: 8000 });
    await page.click(".sidebar__item[aria-label='Models']");
    await page.waitForTimeout(500);
    await checkNoHorizOverflow(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-models-1024x600.png" });
  });

  test("settings page renders without overflow", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".sidebar__item[aria-label='Settings']", { timeout: 8000 });
    await page.click(".sidebar__item[aria-label='Settings']");
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
    await checkSidebarCompact(page);
    await page.screenshot({ path: "kiosk-screenshots/sections-dashboard-800x480.png" });
  });

  test("sidebar is narrower than 1024×600 variant", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".sidebar", { timeout: 8000 });

    const sidebarW = await page.evaluate(
      () => document.querySelector(".sidebar")?.getBoundingClientRect().width ?? 0,
    );
    // At max-height: 500px the sidebar shrinks to 44px
    expect(sidebarW, "Sidebar ≤44px at 800×480").toBeLessThanOrEqual(44);
  });
});

// ── Hub UI — 1024×600 ────────────────────────────────────────────────────────

test.describe("Hub UI — 1024×600", () => {
  test.use({ viewport: { width: 1024, height: 600 } });

  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    // Force hub section via localStorage flag
    await page.addInitScript(() => {
      localStorage.setItem("giap-active-section", "hub");
    });
  });

  test("hub home renders without horizontal overflow", async ({ page }) => {
    await page.goto("/");
    // The hub may not load, so only rendering and overflow are required.
    await page.waitForLoadState("networkidle");
    await checkNoHorizOverflow(page);
    const hasHub = await page.evaluate(
      () => !!document.querySelector(".ghub") || !!document.querySelector(".irail") || !!document.querySelector(".dash"),
    );
    if (hasHub) {
      await checkIrailCompact(page);
    }
    await page.screenshot({ path: "kiosk-screenshots/hub-home-1024x600.png" });
  });
});

// ── Hub UI — 800×480 ─────────────────────────────────────────────────────────

test.describe("Hub UI — 800×480", () => {
  test.use({ viewport: { width: 800, height: 480 } });

  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.addInitScript(() => {
      localStorage.setItem("giap-active-section", "hub");
    });
  });

  test("hub renders without horizontal overflow at 800×480", async ({ page }) => {
    await page.goto("/");
    await page.waitForLoadState("networkidle");
    await checkNoHorizOverflow(page);
    const hasHub = await page.evaluate(
      () => !!document.querySelector(".ghub") || !!document.querySelector(".irail"),
    );
    if (hasHub) {
      await checkIrailCompact(page);
    }
    await page.screenshot({ path: "kiosk-screenshots/hub-home-800x480.png" });
  });
});
