/**
 * Playwright E2E tests for the chat-history sidebar (SessionDropdown).
 *
 * Covers the Phase 3 feature: title fallback, message_count badge, inline
 * rename (PATCH /sessions/:id) and delete-with-confirm (DELETE /sessions/:id).
 *
 * All tests use mocked API routes — no running pond-server required.
 * `mockAllApiRoutes` mocks the sessions list/CRUD; this spec overrides the
 * list GET with populated rows and asserts the outgoing rename/delete calls.
 *
 * Run: cd pond-desktop && npx playwright test tests/e2e/sessions-history.spec.ts
 */
import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

const SESSIONS = [
  {
    id: "sess-aaaaaaaa-1111",
    title: "Weekend weather plan",
    message_count: 6,
    total_prompt_tokens: 120,
    total_completion_tokens: 80,
    model_name: "llama3.2",
    // Recent so it lands in the "Today" group.
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
  },
  {
    // No title → the backend would supply a derived label; here we send none so
    // the client fallback (`Session <id8>`) is exercised.
    id: "sess-bbbbbbbb-2222",
    message_count: 2,
    total_prompt_tokens: 10,
    total_completion_tokens: 5,
    model_name: "llama3.2",
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
  },
];

async function goToChat(page: Parameters<typeof mockAllApiRoutes>[0]) {
  await page.goto("/");
  const chatBtn = page
    .getByRole("button", { name: /chat/i })
    .or(page.locator('[title="Chat"]'))
    .first();
  await chatBtn.click({ timeout: 10_000 });
}

async function openHistory(page: Parameters<typeof mockAllApiRoutes>[0]) {
  // No toggle to click any more: the conversation list is part of the chat
  // sidebar rather than a panel behind a "session history" button, and that
  // button no longer exists anywhere in the source. Waiting on the heading
  // still pins the thing these tests actually need — that the list rendered.
  await expect(
    page.getByRole("heading", { name: "Conversations" }),
  ).toBeVisible({ timeout: 10_000 });
}

test.describe("Chat history sidebar", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    // Override the list with populated rows (must be registered after
    // mockAllApiRoutes so it takes precedence).
    await page.route("**/api/v1/sessions", (route) => {
      if (route.request().method() === "GET") {
        return route.fulfill({ json: { sessions: SESSIONS } });
      }
      return route.fallback();
    });
  });

  test("renders titles, id fallback, and message_count badge", async ({ page }) => {
    await goToChat(page);
    await openHistory(page);

    // Stored title renders verbatim.
    await expect(page.getByText("Weekend weather plan")).toBeVisible();
    // A titleless session falls back to "Untitled" (ChatHistory.tsx). The claim
    // that matters is unchanged and still asserted below: never the raw id.
    await expect(page.getByText("Untitled")).toBeVisible();
    await expect(page.getByText("sess-bbbbbbbb-2222", { exact: true })).toHaveCount(0);
    // The count is card text now rather than a `title` attribute.
    await expect(page.getByText("6 messages")).toBeVisible();
    await expect(page.getByText("2 messages")).toBeVisible();
  });

  test("inline rename issues PATCH with the new title", async ({ page }) => {
    let patchBody: Record<string, unknown> | null = null;
    await page.route("**/api/v1/sessions/*", (route) => {
      const url = route.request().url();
      if (url.includes("/messages")) return route.fallback();
      if (route.request().method() === "PATCH") {
        patchBody = (route.request().postDataJSON() ?? {}) as Record<string, unknown>;
        return route.fulfill({ json: { session_id: "sess-aaaaaaaa-1111", title: patchBody.title } });
      }
      return route.fallback();
    });

    await goToChat(page);
    await openHistory(page);

    // Renaming is no longer a row action. The chat header's title carries the
    // edit, and Chat.tsx notes it is "the ONLY way to set a name by hand now
    // that the history dropdown is gone" — so the conversation has to be open
    // for there to be a name to set. The behaviour under test is unchanged:
    // a hand-typed name issues the PATCH.
    await page
      .getByRole("button", { name: /Open conversation: Weekend weather plan/ })
      .click();
    await page.getByRole("button", { name: /Weekend weather plan/ }).click();

    const editInput = page.getByRole("textbox", { name: "Conversation name" });
    await expect(editInput).toBeVisible();
    await editInput.fill("Trip planning");
    await editInput.press("Enter");

    await expect.poll(() => patchBody).not.toBeNull();
    expect(patchBody).toEqual({ title: "Trip planning" });
  });

  test("delete requires confirmation then issues DELETE", async ({ page }) => {
    let deleteUrl: string | null = null;
    await page.route("**/api/v1/sessions/*", (route) => {
      const url = route.request().url();
      if (url.includes("/messages")) return route.fallback();
      if (route.request().method() === "DELETE") {
        deleteUrl = url;
        return route.fulfill({ status: 204, body: "" });
      }
      return route.fallback();
    });

    await goToChat(page);
    await openHistory(page);

    // The actions are always in the DOM now rather than revealed on hover, so
    // there is nothing to hover first.
    const row = page.locator(".chist__card").filter({ hasText: "Weekend weather plan" });
    await row.locator(".chist__del").click();

    // The confirmation is an inline Delete/Keep pair rather than a prompt, so
    // the assertion is that it appeared and that nothing has been deleted yet.
    await expect(row.locator(".chist__confirm")).toBeVisible();
    expect(deleteUrl).toBeNull();

    // Confirm.
    await row.locator(".chist__confirmYes").click();

    await expect.poll(() => deleteUrl).not.toBeNull();
    expect(deleteUrl).toContain("/api/v1/sessions/sess-aaaaaaaa-1111");
  });
});
