/** Chat section E2E against mocked API routes; the stream mock is overridden per test. */
import { test, expect } from "@playwright/test";
import { mockAllApiRoutes, mockSseStream } from "./helpers/api-mocks";

// ── Helper: build a mock SSE body ─────────────────────────────────────────────

function sseStream(
  textContent: string,
  opts: {
    model_role?: string;
    usage?: { prompt_tokens: number; completion_tokens: number };
    error?: string;
  } = {},
): string {
  if (opts.error) {
    return mockSseStream([{ error: opts.error }]);
  }

  const donePayload: Record<string, unknown> = {
    done: true,
    session_id: "e2e-session",
    model_role: opts.model_role ?? "chat",
  };
  if (opts.usage) {
    donePayload.usage = {
      prompt_tokens: opts.usage.prompt_tokens,
      completion_tokens: opts.usage.completion_tokens,
    };
  }

  return mockSseStream([
    { type: "text", content: textContent, token: textContent },
    donePayload,
  ]);
}

// ── Navigate to the Chat section ──────────────────────────────────────────────

async function goToChat(page: Parameters<typeof mockAllApiRoutes>[0]) {
  await page.goto("/");
  const chatBtn = page
    .getByRole("button", { name: /chat/i })
    .or(page.locator('[title="Chat"]'))
    .first();
  await chatBtn.click({ timeout: 10_000 });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

test.describe("Chat section — response rendering", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("chat response renders in agent bubble", async ({ page }) => {
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: sseStream("Hello from Pond, I am your assistant."),
      }),
    );

    await goToChat(page);

    const textarea = page.locator("textarea").first();
    await textarea.fill("Hello");
    await textarea.press("Meta+Enter");

    await expect(page.getByText("Hello from Pond, I am your assistant.")).toBeVisible({
      timeout: 10_000,
    });
  });

  test("think role badge appears on agent bubble", async ({ page }) => {
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: sseStream("Light scatters due to Rayleigh scattering.", { model_role: "think" }),
      }),
    );

    await goToChat(page);
    const textarea = page.locator("textarea").first();
    await textarea.fill("explain why the sky is blue");
    await textarea.press("Meta+Enter");

    await expect(page.getByText(/think/i)).toBeVisible({ timeout: 10_000 });
  });

  test("task role badge appears on agent bubble", async ({ page }) => {
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: sseStream("Reminder set for 9 AM.", { model_role: "task" }),
      }),
    );

    await goToChat(page);
    const textarea = page.locator("textarea").first();
    await textarea.fill("remind me to call mum at 9am");
    await textarea.press("Meta+Enter");

    await expect(page.getByText(/task/i)).toBeVisible({ timeout: 10_000 });
  });

  test("tool_call event renders before task response", async ({ page }) => {
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: mockSseStream([
          { type: "tool_call", tool: "giap__get_current_weather", result: { temperature: 24, condition: "Sunny" } },
          { type: "text", content: "Set. It will be sunny.", token: "Set. It will be sunny." },
          { done: true, session_id: "e2e-session", model_role: "task" },
        ]),
      }),
    );

    await goToChat(page);
    const textarea = page.locator("textarea").first();
    await textarea.fill("remind me to walk at sunset if weather is clear");
    await textarea.press("Meta+Enter");

    await expect(page.getByText(/Set\. It will be sunny\./i)).toBeVisible({ timeout: 10_000 });
    // Tool result appears as a chip or inline element (not necessarily role=article)
    await expect(page.getByText(/weather|sunny/i).first()).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(/task/i)).toBeVisible({ timeout: 10_000 });
  });

  test("token usage appears in model role badge when reported", async ({ page }) => {
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: sseStream("I'm counting tokens!", {
          model_role: "chat",
          usage: { prompt_tokens: 10, completion_tokens: 47 },
        }),
      }),
    );

    await goToChat(page);
    const textarea = page.locator("textarea").first();
    await textarea.fill("hello");
    await textarea.press("Meta+Enter");

    await expect(page.getByText(/47 tokens/i)).toBeVisible({ timeout: 10_000 });
  });

  test("error event shows error text in bubble", async ({ page }) => {
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: sseStream("", { error: "llamafile stream request failed: connection refused" }),
      }),
    );

    await goToChat(page);
    const textarea = page.locator("textarea").first();
    await textarea.fill("hello");
    await textarea.press("Meta+Enter");

    await expect(
      page.getByText(/error|llamafile|connection refused/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("multi-turn conversation: both user messages visible", async ({ page }) => {
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: sseStream("Acknowledged."),
      }),
    );

    await goToChat(page);
    const textarea = page.locator("textarea").first();

    await textarea.fill("First message");
    await textarea.press("Meta+Enter");
    await page.waitForTimeout(500);

    await textarea.fill("Second message");
    await textarea.press("Meta+Enter");

    await expect(page.getByText("First message")).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText("Second message")).toBeVisible({ timeout: 10_000 });
  });

  // mockAllApiRoutes answers `sessions/*/compact` with a `not_under_pressure` refusal.
  test("context_warning renders the pressure note and its refusal", async ({ page }) => {
    await page.route("**/api/v1/chat/stream", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: mockSseStream([
          { type: "text", content: "The greenhouse fans are on.", token: "The greenhouse fans are on." },
          {
            type: "context_warning",
            utilization_pct: 82.4,
            turns_remaining: 2,
            avg_growth_rate: 640,
            warning: "Context window 82% full (6750/8192 tokens). ~2 turns remaining.",
          },
          { done: true, session_id: "e2e-session", model_role: "chat" },
        ]),
      }),
    );

    await goToChat(page);
    const textarea = page.locator("textarea").first();
    await textarea.fill("are the fans on?");
    await textarea.press("Meta+Enter");

    await expect(page.locator(".ctx-pressure")).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(/82% full/)).toBeVisible({ timeout: 10_000 });

    // The refusal is the common answer and must read as information, not failure.
    await page.getByRole("button", { name: /compact now/i }).click();
    await expect(page.locator(".ctx-pressure__result")).toHaveText(
      /still room in this window/i,
      { timeout: 10_000 },
    );
  });
});

// ── Leaving the section mid-turn ──────────────────────────────────────────────

/** The one test driving the real unmount: `GuiMode` destroys `<Chat />` on a section switch. */
test.describe("Chat section — a turn survives leaving the section", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  async function goToSection(page: Parameters<typeof mockAllApiRoutes>[0], name: RegExp) {
    await page.getByRole("button", { name }).or(page.locator(`[title="${name.source}"]`)).first().click();
  }

  test("the answer that lands while you are on another section is there when you return", async ({ page }) => {
    // Held open until released, so "still answering" is a real state, not a race.
    let release: () => void = () => {};
    const held = new Promise<void>((r) => { release = r; });
    await page.route("**/api/v1/chat/stream", async (route) => {
      await held;
      await route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: sseStream("Geese fly in a V to save energy."),
      });
    });

    await goToChat(page);
    const textarea = page.locator("textarea").first();
    await textarea.fill("why do geese fly in a V");
    await textarea.press("Meta+Enter");

    // Leave while it is still working.
    await goToSection(page, /devices/i);
    await expect(page.locator("textarea")).toHaveCount(0);

    // It finishes with nothing mounted to receive it.
    release();
    await page.waitForTimeout(500);

    await goToSection(page, /chat/i);
    // The thread, not the wall, and the answer is in it.
    await expect(page.getByText("Geese fly in a V to save energy.")).toBeVisible({
      timeout: 10_000,
    });
  });

  test("a turn still running is still running when you come back", async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((r) => { release = r; });
    await page.route("**/api/v1/chat/stream", async (route) => {
      await held;
      await route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
        body: sseStream("Finished after all."),
      });
    });

    await goToChat(page);
    const textarea = page.locator("textarea").first();
    await textarea.fill("take your time");
    await textarea.press("Meta+Enter");
    await expect(page.getByText("take your time")).toBeVisible();

    await goToSection(page, /devices/i);
    await goToSection(page, /chat/i);

    // Back in the thread, question showing, composer still in queue mode.
    await expect(page.getByText("take your time")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("textarea").first()).toHaveAttribute(
      "placeholder",
      /Queue a message/,
    );

    // And it lands into the thread we came back to.
    release();
    await expect(page.getByText("Finished after all.")).toBeVisible({ timeout: 10_000 });
  });
});


// ── Live E2E tests (require running pond-server) ───────────────────────────────

const LIVE = !!process.env.GIAP_SERVER_URL;

test.describe("Chat — live provider tests", () => {
  test.skip(!LIVE, "Set GIAP_SERVER_URL to run live provider tests");

  test("live chat with provider returns streamed tokens", async ({ page }) => {
    await page.goto(process.env.GIAP_SERVER_URL!);
    const textarea = page.locator("textarea").first();
    await textarea.fill("Hello! What is 2 + 2?");
    await textarea.press("Meta+Enter");

    await expect(
      page.locator("text=/four|4/i").first()
    ).toBeVisible({ timeout: 30_000 });
  });

  test("live think badge shows on reasoning query", async ({ page }) => {
    await page.goto(process.env.GIAP_SERVER_URL!);
    const textarea = page.locator("textarea").first();
    await textarea.fill("Explain in detail why the sky is blue.");
    await textarea.press("Meta+Enter");

    await expect(page.getByText(/think/i)).toBeVisible({ timeout: 30_000 });
  });
});
