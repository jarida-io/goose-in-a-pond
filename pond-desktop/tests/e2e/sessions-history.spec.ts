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
import { navigateTo } from "./helpers/nav";

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
  // Chat lives behind the drawer's "Pond" group now, not on the page.
  await navigateTo(page, "Chat");
}

async function openHistory(page: Parameters<typeof mockAllApiRoutes>[0]) {
  await page.getByRole("button", { name: /session history/i }).click();
  await expect(page.getByText("Conversations")).toBeVisible({ timeout: 10_000 });
}

// Parked, and not by the drawer.
//
// Every test below drives `SessionDropdown` — the "session history" trigger,
// `.session-dropdown__item`, the inline rename textbox, the delete-confirm. No
// screen renders that component any more: 595abd4e (2026-08-15) replaced the
// dropdown with the `ChatHistory` wall, which Chat shows by default and whose
// only way back is the header's "All conversations". `grep -rn SessionDropdown
// src` finds the file and one comment, no call site.
//
// So this is not a navigation failure and there is no honest migration: the
// wall has no rename control and labels a titleless session "Untitled" rather
// than "Session <id8>", so re-pointing these would mean rewriting what they
// assert. Parked whole rather than deleted — the behaviour it covers (title
// fallback, message_count badge, PATCH rename, DELETE with confirm) still needs
// an E2E, written against the wall.
test.describe.fixme("Chat history sidebar", () => {
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
    // Titleless session falls back to a short id label — never the full id.
    await expect(page.getByText(/^Session sess-bbb/)).toBeVisible();
    await expect(page.getByText("sess-bbbbbbbb-2222", { exact: true })).toHaveCount(0);
    // Badge shows the message count.
    await expect(page.getByTitle("6 messages")).toBeVisible();
    await expect(page.getByTitle("2 messages")).toBeVisible();
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

    // Hover the first row to reveal the actions, then click rename.
    const row = page.locator(".session-dropdown__item").filter({ hasText: "Weekend weather plan" });
    await row.hover();
    await row.getByRole("button", { name: /rename conversation/i }).click();

    const editInput = page.getByRole("textbox", { name: /rename conversation/i });
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

    const row = page.locator(".session-dropdown__item").filter({ hasText: "Weekend weather plan" });
    await row.hover();
    await row.getByRole("button", { name: /delete conversation/i }).click();

    // A confirmation step appears — nothing deleted yet.
    await expect(page.getByText(/delete this conversation\?/i)).toBeVisible();
    expect(deleteUrl).toBeNull();

    // Confirm.
    await page.getByRole("button", { name: /confirm delete/i }).click();

    await expect.poll(() => deleteUrl).not.toBeNull();
    expect(deleteUrl).toContain("/api/v1/sessions/sess-aaaaaaaa-1111");
  });
});
