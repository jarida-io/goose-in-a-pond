/** A web search turn in Chat: the SSE `ui` hint reaches the result card, and its links leave the app. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { mockAllApiRoutes, mockSseStream } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import { contrastOf } from "./helpers/contrast";

const RESULTS = [
  {
    title: "Opening hours - Example Town Library",
    url: "https://library.example.org/visit/hours",
    domain: "library.example.org",
    snippet: "Saturday 9:00 to 13:00. Closed on public holidays.",
  },
  {
    title: "Visiting the library",
    url: "https://www.example.com/council/library",
    domain: "example.com",
    snippet: "The reading room keeps the same hours on weekends.",
  },
];

test.describe("Chat: web search result card", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: mockSseStream([
          { type: "tool_call", id: "call-1", tool: "giap-knowledge__search_web", result: {} },
          {
            type: "tool_result",
            id: "call-1",
            tool: "giap-knowledge__search_web",
            content: "Web results for: when does the library open on saturdays",
            ui: {
              card_type: "search",
              data: { query: "when does the library open on saturdays", results: RESULTS },
            },
          },
          { type: "text", content: "It opens at nine.", token: "It opens at nine." },
          { done: true, session_id: "e2e-session", model_role: "chat" },
        ]),
      }),
    );
    await page.context().route("https://library.example.org/**", (r) =>
      r.fulfill({ contentType: "text/html", body: "<title>Library hours stand-in</title>" }),
    );
  });

  test("the turn gets a Web search chip that opens to results, each a link out", async ({ page }) => {
    await page.goto("/");
    await navigateTo(page, "Chat");
    const textarea = page.locator("textarea").first();
    await textarea.fill("when does the library open on saturdays");
    await textarea.press("Meta+Enter");

    await expect(page.getByText("It opens at nine.")).toBeVisible({ timeout: 10_000 });
    const chip = page.getByRole("button", { name: /^Web search/ });
    await expect(chip).toBeVisible();
    await chip.click();

    const first = page.getByRole("link", { name: /Opening hours - Example Town Library/ });
    await expect(first).toHaveAttribute("href", "https://library.example.org/visit/hours");
    await expect(page.getByText("Saturday 9:00 to 13:00. Closed on public holidays.")).toBeVisible();
    await expect(page.getByText("library.example.org")).toBeVisible();

    const opened = page.waitForEvent("popup");
    await first.click();
    const popup = await opened;
    expect(popup.url()).toContain("library.example.org");
    await expect(popup).toHaveTitle("Library hours stand-in");
    // The click went to the link, not to the chip's own toggle.
    await expect(first).toBeVisible();
  });
});

for (const scheme of ["Light", "Dark"]) {
  test(`a result link reads at 4.5:1, ${scheme}`, async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.addInitScript((theme) => localStorage.setItem("goosehub_theme", theme), scheme);
    const stream = mockSseStream([
      { type: "tool_call", id: "call-1", tool: "giap-knowledge__search_web", result: {} },
      {
        type: "tool_result",
        id: "call-1",
        tool: "giap-knowledge__search_web",
        content: "Web results for: q",
        ui: { card_type: "search", data: { query: "q", results: RESULTS } },
      },
      { type: "text", content: "ok", token: "ok" },
      { done: true, session_id: "e2e-session", model_role: "chat" },
    ]);
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({ status: 200, headers: { "Content-Type": "text/event-stream" }, body: stream }),
    );
    await page.goto("/");
    await navigateTo(page, "Chat");
    const textarea = page.locator("textarea").first();
    await textarea.fill("q");
    await textarea.press("Meta+Enter");
    await page.getByRole("button", { name: /^Web search/ }).click();
    const link = page.getByRole("link", { name: /Opening hours/ });
    await expect(link).toBeVisible({ timeout: 10_000 });

    expect(await page.evaluate(() => document.documentElement.dataset.theme)).toBe(scheme.toLowerCase());
    expect(await contrastOf(link)).toBeGreaterThanOrEqual(4.5);
  });
}

const PLACES = [
  {
    name: "Java House Junction",
    url: "https://www.javahouseafrica.com/junction",
    snippet: "day: Monday; hours: 7 am - 10 pm",
  },
  { name: "Artcaffe Junction", url: "", snippet: "" },
];

const STUB = "Web search results are not kept after the turn. Sources: javahouseafrica.com, thejunction.co.ke.";

function placesStream() {
  return mockSseStream([
    { type: "tool_call", id: "call-1", tool: "giap-knowledge__search_web", result: {} },
    {
      type: "tool_result",
      id: "call-1",
      tool: "giap-knowledge__search_web",
      content: "Web results for: when does java house at junction close",
      transient: true,
      ui: {
        card_type: "search",
        data: { query: "when does java house at junction close", results: RESULTS, places: PLACES },
      },
    },
    { type: "text", content: "It closes at ten.", token: "It closes at ten." },
    { done: true, session_id: "e2e-session", model_role: "chat" },
  ]);
}

async function askAboutPlaces(page: Page) {
  await page.route("**/api/v1/chat/stream", (route) =>
    route.fulfill({ status: 200, headers: { "Content-Type": "text/event-stream" }, body: placesStream() }),
  );
  await page.goto("/");
  await navigateTo(page, "Chat");
  const textarea = page.locator("textarea").first();
  await textarea.fill("when does java house at junction close");
  await textarea.press("Meta+Enter");
  await expect(page.getByText("It closes at ten.")).toBeVisible({ timeout: 10_000 });
  await page.getByRole("button", { name: /^Web search/ }).click();
}

test.describe("Chat: nearby places on the result card", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("lists the places above the pages, linking only an address that is a web address", async ({ page }) => {
    await askAboutPlaces(page);

    const places = page.getByRole("list", { name: "Nearby places" });
    await expect(places.getByRole("listitem")).toHaveCount(2);
    await expect(page.getByText("Nearby places", { exact: true })).toBeVisible();
    await expect(page.getByText("day: Monday; hours: 7 am - 10 pm")).toBeVisible();
    await expect(places.getByRole("link")).toHaveCount(1);
    await expect(places.getByRole("link", { name: /Java House Junction/ })).toHaveAttribute(
      "href",
      "https://www.javahouseafrica.com/junction",
    );
    await expect(places.getByText("Artcaffe Junction")).toBeVisible();

    const order = await page.locator(".ui-search").evaluate((card) => {
      const text = card.textContent ?? "";
      return text.indexOf("Java House Junction") < text.indexOf("Opening hours - Example Town Library");
    });
    expect(order).toBe(true);
  });

  test("does not write the result to the browser's storage, since the pond keeps only a stub", async ({ page }) => {
    await askAboutPlaces(page);
    const stored = await page.evaluate(() =>
      JSON.stringify([{ ...localStorage }, { ...sessionStorage }]),
    );
    for (const text of ["Java House", "Opening hours - Example Town Library", "javahouseafrica", "library.example.org"]) {
      expect(stored).not.toContain(text);
    }
  });

  for (const scheme of ["Light", "Dark"]) {
    test(`a place link, its group label and its details read at 4.5:1, ${scheme}`, async ({ page }) => {
      await page.addInitScript((theme) => localStorage.setItem("goosehub_theme", theme), scheme);
      await askAboutPlaces(page);
      const link = page.getByRole("link", { name: /Java House Junction/ });
      await expect(link).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.dataset.theme)).toBe(scheme.toLowerCase());
      expect(await contrastOf(link), "the place link").toBeGreaterThanOrEqual(4.5);
      for (const part of [".ui-search__label", ".ui-search__domain", ".ui-search__snippet"]) {
        expect(await contrastOf(page.locator(part).first()), part).toBeGreaterThanOrEqual(4.5);
      }
    });
  }
});

test.describe("Chat: a reloaded conversation whose search results were not kept", () => {
  async function reload(page: Page, rows: unknown[]) {
    await mockAllApiRoutes(page);
    await page.route("**/api/v1/sessions/*/messages", (route) => route.fulfill({ json: { messages: rows } }));
    await page.route("**/api/v1/sessions/*/active-run", (route) => route.fulfill({ status: 404, json: { error: "none" } }));
    // The run pointer is how a window finds the conversation it was last showing.
    await page.addInitScript(() => {
      localStorage.setItem("giap-section", "chat");
      localStorage.setItem(
        "giap-chat-run",
        JSON.stringify({ sessionId: "e2e-session", runId: "run-1", epoch: "e", lastSeq: 4 }),
      );
    });
    await page.goto("/");
  }

  const base = { session_id: "e2e-session", created_at: new Date().toISOString() };
  const rows = [
    { ...base, id: "u1", role: "user", content: "when does java house at junction close" },
    {
      ...base,
      id: "t1",
      role: "tool",
      tool_call_id: "call-1",
      content: JSON.stringify({
        tool_call_id: "call-1",
        tool: "giap-knowledge__search_web",
        content: STUB,
        arguments: '{"question":"when does java house at junction close"}',
        transient: true,
      }),
    },
    {
      ...base,
      id: "a1",
      role: "assistant",
      content: "It closes at ten.",
      tool_calls: [{ id: "call-1", name: "giap-knowledge__search_web", arguments: "{}" }],
    },
  ];

  test("shows one quiet line naming the sources, not an empty card", async ({ page }) => {
    await reload(page, rows);
    await expect(page.getByText("It closes at ten.")).toBeVisible({ timeout: 10_000 });
    const note = page.locator(".kept-note");
    await expect(note).toHaveText(
      "Web search: results aren't kept after the turn. Sources: javahouseafrica.com, thejunction.co.ke.",
    );
    await expect(page.locator(".ui-search, .tool-call-card")).toHaveCount(0);
    await expect(page.getByRole("button", { name: /^Web search/ })).toHaveCount(0);
  });

  for (const scheme of ["Light", "Dark"]) {
    test(`the line reads at 4.5:1, ${scheme}`, async ({ page }) => {
      await page.addInitScript((theme) => localStorage.setItem("goosehub_theme", theme), scheme);
      await reload(page, rows);
      const note = page.locator(".kept-note");
      await expect(note).toBeVisible({ timeout: 10_000 });
      expect(await contrastOf(note)).toBeGreaterThanOrEqual(4.5);
    });
  }
});
