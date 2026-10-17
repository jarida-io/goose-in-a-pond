/** A web search turn in Chat: the SSE `ui` hint reaches the result card, and its links leave the app. */
import { test, expect } from "@playwright/test";
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
