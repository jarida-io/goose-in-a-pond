/** Chat-history sidebar: title fallback, message counts, rename (PATCH) and delete (DELETE). */
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
    // No title, so the client fallback is exercised.
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
  // The list is part of the chat sidebar; its heading confirms it rendered.
  await expect(
    page.getByRole("heading", { name: "Conversations" }),
  ).toBeVisible({ timeout: 10_000 });
}

test.describe("Chat history sidebar", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    // After mockAllApiRoutes, so it takes precedence.
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

    await expect(page.getByText("Weekend weather plan")).toBeVisible();
    // A titleless session shows "Untitled", never the raw id.
    await expect(page.getByText("Untitled")).toBeVisible();
    await expect(page.getByText("sess-bbbbbbbb-2222", { exact: true })).toHaveCount(0);
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

    // Rename lives on the open conversation's header title, so open it first.
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

    const row = page.locator(".chist__card").filter({ hasText: "Weekend weather plan" });
    await row.locator(".chist__del").click();

    // Inline Delete/Keep confirmation; nothing is deleted until confirmed.
    await expect(row.locator(".chist__confirm")).toBeVisible();
    expect(deleteUrl).toBeNull();

    await row.locator(".chist__confirmYes").click();

    await expect.poll(() => deleteUrl).not.toBeNull();
    expect(deleteUrl).toContain("/api/v1/sessions/sess-aaaaaaaa-1111");
  });
});
