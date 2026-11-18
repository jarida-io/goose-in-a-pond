/** Idea cards: the deck on My Goose and Home, its actions, and the kiosk panel's fit and targets. */
import { test, expect, type Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { mockMyGoose } from "./helpers/my-goose-mocks";
import { mockCards } from "./helpers/card-mocks";
import { navigateTo } from "./helpers/nav";

async function openMyGoose(page: Page): Promise<void> {
  await page.goto("/");
  await navigateTo(page, "My Goose");
  await expect(page.getByRole("heading", { level: 1, name: "My Goose" })).toBeVisible();
}

test.describe("Idea cards in the classic shell", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await mockMyGoose(page);
  });

  test("My Goose lists the cards; one is done, one turned down with reasons", async ({ page }) => {
    const calls = await mockCards(page);
    await openMyGoose(page);

    const deck = page.getByRole("region", { name: "Ideas for you" });
    await expect(deck.getByText("Ready")).toBeVisible();
    const walk = deck.getByRole("article", { name: "Walk the Karura Forest waterfall trail" });
    await expect(walk.getByText("From your interest: Hiking")).toBeVisible();
    await expect(walk.getByText("KSh 600 entry")).toBeVisible();

    await walk.getByRole("button", { name: "Details" }).click();
    await expect(walk.getByText("The waterfall trail is about 5 km.")).toBeVisible();
    await expect(walk.getByRole("link", { name: /Karura Forest: visiting/ })).toHaveAttribute("href", "https://www.karurafriends.org/visit");

    await walk.getByRole("button", { name: "Add to-do" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Added to your to-dos" })).toBeVisible();
    await expect(walk).toHaveCount(0);
    expect(calls.done.map((d) => d.id)).toEqual(["c-walk"]);

    const pack = deck.getByRole("article", { name: "What should we pack for Diani?" });
    await pack.getByRole("button", { name: "Not this" }).click();
    const sheet = page.getByRole("dialog", { name: "Not this one" });
    await sheet.getByRole("button", { name: "I already know this" }).click();
    await sheet.getByRole("textbox", { name: "What was wrong?" }).fill("We have a list");
    await sheet.getByRole("button", { name: "Not this one" }).click();
    await expect(pack).toHaveCount(0);
    expect(calls.dismissed).toEqual([{ id: "c-pack", body: { reasons: ["already_know"], text: "We have a list" } }]);
    await expect(deck.getByText(/Refresh, and Goose makes a few ideas/)).toBeVisible();
  });

  test("a chat card sends its prompt and opens chat", async ({ page }) => {
    const calls = await mockCards(page);
    await openMyGoose(page);
    await page.getByRole("button", { name: "Ask Goose" }).click();
    await expect(page.getByText("What should we pack for Diani?").first()).toBeVisible();
    await expect(page.getByRole("heading", { level: 1, name: "My Goose" })).toHaveCount(0);
    expect(calls.done.map((d) => d.id)).toEqual(["c-pack"]);
  });

  test("Home shows one card at a time and refreshes the set", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    const calls = await mockCards(page);
    await page.goto("/");
    const deck = page.getByRole("region", { name: "Ideas for you" });
    await expect(deck.getByText("Card 1 of 2")).toBeVisible();
    await deck.getByRole("button", { name: "Next" }).click();
    await expect(deck.getByText("What should we pack for Diani?")).toBeVisible();
    await deck.getByRole("button", { name: "Go deeper" }).click();
    await expect(deck.getByText("Walk the Karura Forest waterfall trail")).toBeVisible();
    expect(calls.deeper).toEqual(["c-pack"]);
    await deck.getByRole("button", { name: "Refresh" }).click();
    await expect(deck.getByText("Making your cards")).toBeVisible();
    expect(calls.refreshed).toBe(1);
  });

  // A card with Details open took the whole screen: Home did not scroll, so the widgets under it
  // were cut to a strip nothing could reach. Then a fix gave the top row its own scroll box, which
  // split Home in two. It is one page that scrolls, the widgets whole under the cards.
  test("Home scrolls as one page under a long card, the widgets whole below it", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await mockCards(page);
    await page.goto("/");
    const deck = page.getByRole("region", { name: "Ideas for you" });
    await expect(deck.getByText("Card 1 of 2")).toBeVisible();
    await deck.getByRole("button", { name: "Details" }).click();

    const box = await page.evaluate(() => {
      const dash = document.querySelector<HTMLElement>(".dash")!;
      const top = document.querySelector<HTMLElement>(".dash__top-row")!;
      const body = document.querySelector<HTMLElement>(".dash__body")!;
      return {
        body: body.getBoundingClientRect().height,
        dashScrolls: getComputedStyle(dash).overflowY,
        topScrolls: getComputedStyle(top).overflowY,
        topClipped: top.scrollHeight > top.clientHeight + 1,
      };
    });
    expect(box.body).toBeGreaterThanOrEqual(440);
    expect(box.dashScrolls).toBe("auto");
    expect(box.topScrolls).toBe("visible");
    expect(box.topClipped).toBe(false);

    await page.locator(".dash__track").scrollIntoViewIfNeeded();
    await expect(page.locator(".dash__track")).toBeInViewport({ ratio: 0.9 });
  });
});

/** Every visible deck control clears 44px, by its own box or its `.reach` band. */
async function smallDeckTargets(page: Page): Promise<Array<{ text: string; h: number }>> {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>(".cd button"))
      .filter((el) => el.getClientRects().length > 0)
      .map((el) => ({
        text: (el.textContent ?? "").trim().slice(0, 30),
        h: el.matches(".ink-reach, .reach") ? 44 : Math.round(el.getBoundingClientRect().height),
      }))
      .filter((t) => t.h < 44),
  );
}

for (const viewport of [
  { width: 1024, height: 600 },
  { width: 800, height: 480 },
]) {
  test.describe(`Idea cards on the hub, ${viewport.width}x${viewport.height}`, () => {
    test.use({ viewport });

    test.beforeEach(async ({ page }) => {
      await mockAllApiRoutes(page);
      await mockMyGoose(page);
      await mockCards(page);
      await page.addInitScript(() => {
        localStorage.setItem("giap-section", "hub");
        localStorage.setItem("giap-force-hub", "1");
      });
    });

    test("Home keeps its controls on the panel: one 44px line that leads to the cards", async ({ page }) => {
      await page.goto("/");
      await page.waitForSelector(".ghub", { timeout: 10_000 });
      const glance = page.getByRole("button", { name: /Ideas for you.*2 cards/ });
      await expect(glance).toBeVisible();
      const fit = await page.evaluate(() => ({
        overflow: document.body.scrollWidth > window.innerWidth,
        mic: document.querySelector(".dash__voice")?.getBoundingClientRect().bottom ?? Infinity,
        glance: document.querySelector(".cd-glance")?.getBoundingClientRect().height ?? 0,
        vh: window.innerHeight,
      }));
      expect(fit.overflow, "No horizontal overflow").toBe(false);
      expect(fit.mic, "Talk to Goose is reachable").toBeLessThanOrEqual(fit.vh);
      expect(fit.glance, "The glance is a 44px target").toBeGreaterThanOrEqual(44);

      await glance.click();
      await expect(page.getByRole("heading", { level: 1, name: "My Goose" })).toBeVisible();
      await expect(page.getByRole("region", { name: "Ideas for you" }).getByText("Card 1 of 2")).toBeVisible();
    });

    test("My Goose shows one card at a time with 44px targets", async ({ page }) => {
      await page.goto("/");
      await page.waitForSelector(".ghub", { timeout: 10_000 });
      await navigateTo(page, "My Goose");
      const deck = page.getByRole("region", { name: "Ideas for you" });
      await expect(deck.getByText("Card 1 of 2")).toBeVisible();
      await expect(deck.getByRole("article")).toHaveCount(1);
      expect(await page.evaluate(() => document.body.scrollWidth > window.innerWidth)).toBe(false);
      expect(await smallDeckTargets(page)).toEqual([]);
    });
  });
}
