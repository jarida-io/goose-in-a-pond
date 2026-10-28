/** Pictures, news and videos on a search turn in Chat: thumbnails load only from Brave's image proxy. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { mockAllApiRoutes, mockSseStream } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import { contrastOf } from "./helpers/contrast";

const PROXY = "https://imgs.search.brave.com";
// A 1x1 grey PNG: what the stand-in proxy answers for every thumbnail.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=",
  "base64",
);

const PICTURES = [1, 2, 3].map((i) => ({
  title: `The reading room ${i}`,
  url: `https://library.example.org/pictures/${i}`,
  domain: "library.example.org",
  thumbnail: `${PROXY}/stand-in/${i}`,
  width: 500,
  height: 333,
}));

const NEWS = [
  {
    title: "The library opens a new reading room",
    url: "https://news.example.org/reading-room",
    domain: "news.example.org",
    publisher: "Example News",
    date: "2026-10-26",
    thumbnail: `${PROXY}/stand-in/news`,
    breaking: true,
  },
];

const VIDEOS = [
  {
    title: "A tour of the library",
    url: "https://video.example.org/watch?v=tour",
    domain: "video.example.org",
    duration: "4:12",
    creator: "Example Channel",
    date: null,
    thumbnail: `${PROXY}/stand-in/video`,
  },
];

const RESULTS = [
  {
    title: "Opening hours - Example Town Library",
    url: "https://library.example.org/visit/hours",
    domain: "library.example.org",
    snippet: "Saturday 9:00 to 13:00. Closed on public holidays.",
  },
];

/** Every request the page makes for a picture outside the app itself, wherever it goes. */
async function watchImages(page: Page): Promise<string[]> {
  const seen: string[] = [];
  page.on("request", (r) => {
    const own = new URL(r.url()).origin === new URL(page.url() || "http://localhost:5173").origin;
    if (r.resourceType() === "image" && !own) seen.push(r.url());
  });
  await page.context().route(`${PROXY}/**`, (r) =>
    r.fulfill({ status: 200, contentType: "image/png", body: PNG }),
  );
  return seen;
}

async function ask(page: Page, tool: string, card: Record<string, unknown>, chip: RegExp) {
  await page.route("**/api/v1/chat/stream", (route) =>
    route.fulfill({
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
      body: mockSseStream([
        { type: "tool_call", id: "call-1", tool, result: {} },
        { type: "tool_result", id: "call-1", tool, content: "done", transient: true, ui: card },
        { type: "text", content: "Here you are.", token: "Here you are." },
        { done: true, session_id: "e2e-session", model_role: "chat" },
      ]),
    }),
  );
  await page.goto("/");
  await navigateTo(page, "Chat");
  const textarea = page.locator("textarea").first();
  await textarea.fill("show me the library");
  await textarea.press("Meta+Enter");
  await expect(page.getByText("Here you are.")).toBeVisible({ timeout: 10_000 });
  await page.getByRole("button", { name: chip }).click();
}

const picturesCard = {
  card_type: "pictures",
  data: { query: "pictures of the town library", pictures: PICTURES },
};
const searchCard = {
  card_type: "search",
  data: { query: "the town library", results: RESULTS, news: NEWS, videos: VIDEOS },
};

test.describe("Chat: pictures from the web", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("a Pictures chip opens to a grid of thumbnails, each a link to its page", async ({ page }) => {
    const images = await watchImages(page);
    await ask(page, "giap-knowledge__show_pictures", picturesCard, /^Pictures/);

    const grid = page.getByRole("list", { name: "Pictures: pictures of the town library" });
    await expect(grid.getByRole("listitem")).toHaveCount(3);
    const first = page.getByRole("link", { name: /The reading room 1/ });
    await expect(first).toHaveAttribute("href", "https://library.example.org/pictures/1");
    await expect(page.getByAltText("The reading room 1")).toBeVisible();
    await expect.poll(() => images.length).toBeGreaterThanOrEqual(3);
    expect(images.every((url) => url.startsWith(`${PROXY}/`)), images.join("\n")).toBe(true);
  });

  test("keeps no picture in the browser's storage", async ({ page }) => {
    await watchImages(page);
    await ask(page, "giap-knowledge__show_pictures", picturesCard, /^Pictures/);
    const stored = await page.evaluate(() => JSON.stringify([{ ...localStorage }, { ...sessionStorage }]));
    for (const text of ["reading room", "library.example.org", "imgs.search.brave.com"]) {
      expect(stored).not.toContain(text);
    }
  });
});

test.describe("Chat: news and videos beside the pages", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("shows them under their own labels, with thumbnails only from the proxy", async ({ page }) => {
    const images = await watchImages(page);
    await ask(page, "giap-knowledge__search_web", searchCard, /^Web search/);

    await expect(page.getByRole("list", { name: "News" })).toContainText("The library opens a new reading room");
    await expect(page.getByRole("list", { name: "Videos" })).toContainText("A tour of the library");
    await expect(page.getByText("4:12", { exact: true })).toBeVisible();
    await expect.poll(() => images.length).toBeGreaterThanOrEqual(2);
    expect(images.every((url) => url.startsWith(`${PROXY}/`)), images.join("\n")).toBe(true);
  });

  for (const scheme of ["Light", "Dark"]) {
    test(`news and video text reads at 4.5:1, ${scheme}`, async ({ page }) => {
      await page.addInitScript((theme) => localStorage.setItem("goosehub_theme", theme), scheme);
      await watchImages(page);
      await ask(page, "giap-knowledge__search_web", searchCard, /^Web search/);
      const link = page.getByRole("link", { name: /A tour of the library/ });
      await expect(link).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.dataset.theme)).toBe(scheme.toLowerCase());
      expect(await contrastOf(link), "the video link").toBeGreaterThanOrEqual(4.5);
      for (const part of [".ui-search__label", ".ui-search__domain", ".ui-search__breaking"]) {
        expect(await contrastOf(page.locator(part).first()), part).toBeGreaterThanOrEqual(4.5);
      }
    });
  }
});
