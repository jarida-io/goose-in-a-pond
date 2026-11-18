/** My Goose on the desktop: a candidate confirmed, a plan read, a question answered, something turned down and a plan stopped. */
import { test, expect, type Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { mockMyGoose } from "./helpers/my-goose-mocks";
import { navigateTo } from "./helpers/nav";

async function openMyGoose(page: Page): Promise<void> {
  await page.goto("/");
  await navigateTo(page, "My Goose");
  await expect(page.getByRole("heading", { level: 1, name: "My Goose" })).toBeVisible();
}

test.describe("My Goose in the classic shell", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("a candidate is confirmed and joins what Goose is working on", async ({ page }) => {
    const calls = await mockMyGoose(page);
    await openMyGoose(page);

    await expect(page.getByText("You said you are going on 3 December.")).toBeVisible();
    await page.getByRole("button", { name: "Confirm" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Confirmed: AI Summit Nairobi" })).toBeVisible();
    const working = page.getByRole("list", { name: "What Goose is working on" });
    await expect(working.getByRole("button", { name: /AI Summit Nairobi/ })).toBeVisible();
    expect(calls.confirmed).toEqual(["a-summit"]);
  });

  test("a plan shows its live pipeline, asks inline, and its unsure deliverable reads with the critic's objections", async ({ page }) => {
    const calls = await mockMyGoose(page);
    await openMyGoose(page);

    await page.getByRole("button", { name: /Diani trip/ }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Diani trip" })).toBeVisible();
    const pipeline = page.getByRole("list", { name: "Working on it" });
    await expect(pipeline.getByText("Researching")).toBeVisible();

    await page.getByRole("radio", { name: "Other" }).click();
    await page.getByRole("textbox", { name: "Your answer" }).fill("About KSh 120,000");
    await page.getByRole("button", { name: "Send answer" }).click();
    await expect(page.getByRole("button", { name: "Send answer" })).toHaveCount(0);
    expect(calls.answered).toEqual([{ id: "q-budget", body: { choices: [], text: "About KSh 120,000" } }]);

    await page.getByRole("button", { name: /Plan for Diani trip/ }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Plan for Diani trip" })).toBeVisible();
    const unsure = page.getByRole("region", { name: /isn't sure/ });
    await expect(unsure.getByText("Not backed by a source")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Getting there" })).toBeVisible();
    await expect.poll(() => calls.seen).toEqual(["d-plan"]);

    await page.getByRole("button", { name: "Never this" }).click();
    const sheet = page.getByRole("dialog", { name: "Never this" });
    await sheet.getByRole("button", { name: "Too generic" }).click();
    await sheet.getByRole("textbox", { name: "What was wrong?" }).fill("No beach holidays");
    await sheet.getByRole("button", { name: "Never this" }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Diani trip" })).toBeVisible();
    expect(calls.rejected).toEqual([{ id: "d-plan", body: { verb: "never", reasons: ["too_generic"], text: "No beach holidays" } }]);
  });

  test("stopping a plan sends the reasons and never-again, and leaves it", async ({ page }) => {
    const calls = await mockMyGoose(page);
    await openMyGoose(page);

    await page.getByRole("button", { name: /Diani trip/ }).click();
    await page.getByRole("button", { name: "Stop", exact: true }).click();
    const sheet = page.getByRole("dialog", { name: "Stop Diani trip" });
    await sheet.getByRole("button", { name: "Bad timing" }).click();
    await sheet.getByRole("switch", { name: "Never suggest this again" }).click();
    await sheet.getByRole("button", { name: "Stop it" }).click();

    await expect(page.getByRole("heading", { level: 1, name: "My Goose" })).toBeVisible();
    await expect(page.getByRole("list", { name: "What Goose is working on" })).toHaveCount(0);
    expect(calls.stopped).toEqual([{ id: "a-diani", body: { reasons: ["bad_timing"], never: true } }]);
  });

  test("a pond that cannot tell who is asking points to the phone, once", async ({ page }) => {
    await mockMyGoose(page, { unknownCaller: true });
    await page.goto("/");
    await navigateTo(page, "My Goose");
    await expect(page.getByText("Open My Goose on your phone")).toBeVisible();
    await expect(page.getByRole("tablist")).toHaveCount(0);
    await expect(page.getByRole("alert")).toHaveCount(0);
  });
});

/** Every control a finger drives clears 44px, counting the band a dense control reaches through. */
async function smallTargets(page: Page): Promise<Array<{ text: string; h: number }>> {
  return page.evaluate(() => {
    const controls = Array.from(
      document.querySelectorAll<HTMLElement>(".mg button, .mg [role='radio'], .mg [role='switch'], .mg a[href], dialog[open] button"),
    );
    return controls
      .filter((el) => el.getClientRects().length > 0)
      .map((el) => {
        const band = el.closest<HTMLElement>(".ink-labelled, .mg-reasons__reach");
        const reach = el.matches(".ink-reach, .reach");
        const h = reach ? 44 : Math.round((band ?? el).getBoundingClientRect().height);
        return { text: (el.textContent ?? el.getAttribute("aria-label") ?? "").trim().slice(0, 30), h };
      })
      .filter((t) => t.h < 44);
  });
}

async function noHorizontalOverflow(page: Page): Promise<void> {
  const overflow = await page.evaluate(() => document.body.scrollWidth > window.innerWidth);
  expect(overflow, "No horizontal overflow expected").toBe(false);
}

test.describe("My Goose on the kiosk, 800x480", () => {
  test.use({ viewport: { width: 800, height: 480 } });

  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.addInitScript(() => {
      localStorage.setItem("giap-section", "hub");
      localStorage.setItem("giap-force-hub", "1");
    });
  });

  test("one decision per screen, 44px targets and no sideways scroll through a plan", async ({ page }) => {
    await mockMyGoose(page);
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await navigateTo(page, "My Goose");
    await expect(page.getByRole("heading", { level: 1, name: "My Goose" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Confirm" })).toHaveCount(1);
    await noHorizontalOverflow(page);
    expect(await smallTargets(page)).toEqual([]);

    await page.getByRole("button", { name: /Diani trip/ }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Diani trip" })).toBeVisible();
    await noHorizontalOverflow(page);
    expect(await smallTargets(page)).toEqual([]);

    await page.getByRole("button", { name: "Stop", exact: true }).click();
    await expect(page.getByRole("dialog", { name: "Stop Diani trip" })).toBeVisible();
    await noHorizontalOverflow(page);
    expect(await smallTargets(page)).toEqual([]);
    await page.getByRole("dialog", { name: "Stop Diani trip" }).getByRole("button", { name: "Cancel" }).click();

    await page.getByRole("button", { name: /Plan for Diani trip/ }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Plan for Diani trip" })).toBeVisible();
    await noHorizontalOverflow(page);
    expect(await smallTargets(page)).toEqual([]);
  });

  test("the other parts fit the panel too", async ({ page }) => {
    await mockMyGoose(page);
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await navigateTo(page, "My Goose");
    for (const tab of ["Questions", "About you", "Specialists", "Activity"]) {
      await page.getByRole("tab", { name: tab }).click();
      await expect(page.getByRole("tab", { name: tab })).toHaveAttribute("aria-selected", "true");
      await page.waitForTimeout(200);
      await noHorizontalOverflow(page);
      expect(await smallTargets(page), tab).toEqual([]);
    }
  });
});
