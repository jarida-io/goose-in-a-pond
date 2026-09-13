/**
 * kiosk-viewport.spec.ts
 *
 * Verifies that the GIAP desktop UI renders correctly on 7-inch kiosk panels:
 *   - 1024×600 (primary target)
 *   - 800×480  (secondary / smaller kiosk)
 *
 * Checks:
 *   1. No horizontal overflow (body.scrollWidth === viewport width).
 *   2. Sidebar is in icon-only mode, at the width the design token names.
 *   3. Content area fills the remaining width without clipping.
 *   4. Key interactive targets meet the 40px minimum height.
 *   5. Both classic sections UI (app-shell) and hub UI (ghub/.irail) are
 *      visually verified via screenshot.
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
  // Measured against the token rather than a literal. This assertion was
  // pinned at ≤52px, which is a pixel count standing in for "the rail is
  // icon-only" — so a 4px design change to the rail failed a kiosk test that
  // was not about rail width, and the number in docs/design-system.md drifted
  // out of date at the same time with nothing to catch it. The property this
  // file actually cares about is asserted directly by "sidebar is icon-only
  // (no text labels visible)" below, which checks the brand name and group
  // labels are display:none.
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

  // No WIDER than the token. Equality would be wrong: the 1024x600 rule sets
  // .sidebar { width: var(--sidebar-width-collapsed) }, but the very-short-panel
  // rule (@media max-height:500px, the 800x480 target) narrows it further to a
  // hardcoded 44px. So the token is the ceiling, and anything above it means
  // something is overriding the rail wider than intended — the regression this
  // is for.
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
  // Sidebar nav items should be ≥40px tall
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

    // Group labels should not be visible
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

    // Content should start right where sidebar ends
    expect(contentLeft, "Content starts after sidebar").toBeCloseTo(sidebarW, 1);
    // Content should fill the rest of the viewport
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
    // Hub may or may not load depending on localStorage section flag
    // — just check that the page renders and no overflow
    await page.waitForLoadState("networkidle");
    await checkNoHorizOverflow(page);
    const hasHub = await page.evaluate(
      () => !!document.querySelector(".ghub") || !!document.querySelector(".irail") || !!document.querySelector(".dash"),
    );
    // If hub loaded, verify its icon rail
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
