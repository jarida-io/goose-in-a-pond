/** Memory sub-screen wired to listMemories/deleteMemory, with an offline fallback. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

// ─── Mock data ────────────────────────────────────────────────

const MOCK_MEMORIES = [
  {
    id: "mem-1",
    content: "Prefers the house at 70° morning, 66° overnight.",
    segment: "preference",
    created_at: "2026-06-01T10:00:00Z",
    access_count: 2,
  },
  {
    id: "mem-2",
    content: "Is a software engineer; works from the office most weekdays.",
    segment: "identity",
    created_at: "2026-05-28T08:00:00Z",
    access_count: 5,
  },
  {
    id: "mem-3",
    content: "Lactose intolerant — avoid dairy in recipe suggestions.",
    segment: "preference",
    created_at: "2026-05-24T14:00:00Z",
    access_count: 1,
  },
];

// ─── Route setup ──────────────────────────────────────────────

/** Call before navigating, so the routes are in place. */
async function setupMemoryRoutes(
  page: Page,
  opts: {
    memories?: object[];
    memoriesStatus?: number;
    onDelete?: (id: string) => void;
  } = {},
) {
  const memories = opts.memories ?? MOCK_MEMORIES;
  const memoriesStatus = opts.memoriesStatus ?? 200;

  // ── Core app routes (identical to hub-models-wiring pattern) ──
  await page.route("**/api/v1/health", (r) =>
    r.fulfill({ json: { status: "ok", version: "test" } }),
  );
  await page.route("**/api/v1/handshake", (r) =>
    r.fulfill({ json: { token: "e2e-test-token", session_id: "e2e-session" } }),
  );
  await page.route("**/api/v1/onboard/status", (r) =>
    r.fulfill({ json: { onboarded: true, current_step: "Completed", steps_completed: 9, total_steps: 9 } }),
  );
  await page.route("**/api/v1/onboard/complete", (r) =>
    r.fulfill({ json: { status: "completed" } }),
  );
  await page.route("**/api/v1/settings", (r) =>
    r.fulfill({
      json: {
        assistant_name: "Pond",
        user_name: "Jerry",
        chat_provider: "llamafile",
        chat_model: "llama3.2",
        agent_memory_inject: false,
        prompt_style: "balanced",
        llm_temperature: 0.7,
        llm_max_tokens: 1024,
        memory_consolidation_enabled: false,
      },
    }),
  );
  await page.route("**/api/v1/schedules", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/sessions", (r) => r.fulfill({ json: { sessions: [] } }));
  await page.route("**/api/v1/sessions/*/messages", (r) => r.fulfill({ json: { messages: [] } }));
  await page.route("**/api/v1/devices", (r) => r.fulfill({ json: { devices: [] } }));
  await page.route("**/api/v1/skills", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/prompts", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/prompts/**", (r) =>
    r.fulfill({ json: { name: "balanced", content: "You are a helpful assistant.", is_system: true } }),
  );
  await page.route("**/api/v1/agent/extras", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/agent/tools", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/recipes", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/transcribe", (r) => r.fulfill({ json: { text: "" } }));
  await page.route("**/api/v1/chat/stream", (r) =>
    r.fulfill({ status: 200, headers: { "Content-Type": "text/event-stream" }, body: 'data: {"done":true}\n\n' }),
  );
  await page.route("**/api/v1/tts", (r) => r.fulfill({ status: 503, json: { error: "off" } }));
  await page.route("**/api/v1/profiles", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/extensions", (r) => r.fulfill({ json: { extensions: [] } }));
  await page.route("**/api/v1/extensions/**", (r) => r.fulfill({ json: { status: "ok" } }));
  await page.route("**/api/v1/marketplace", (r) => r.fulfill({ json: { extensions: [] } }));
  await page.route("**/api/v1/marketplace/*/install", (r) => r.fulfill({ status: 201, json: {} }));
  await page.route("**/api/v1/secrets/**", (r) => r.fulfill({ json: { keys: [] } }));
  await page.route("**/api/v1/logs", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/usage", (r) => r.fulfill({ json: { total_tokens: 0, session_count: 0 } }));
  await page.route("**/api/v1/oauth/**", (r) => r.fulfill({ json: {} }));
  await page.route("**/api/v1/models/memory-status", (r) =>
    r.fulfill({ json: { total_mb: 8192, available_for_llm_mb: 4096, loaded_model: null } }),
  );
  await page.route("**/api/v1/models/download/progress", (r) =>
    r.fulfill({ json: { downloads: [] } }),
  );
  await page.route("**/api/v1/models/ollama", (r) => r.fulfill({ json: { models: [] } }));
  await page.route("**/api/v1/models/capabilities", (r) =>
    r.fulfill({ json: { thinking: false, vision: false, audio_input: false, context_window_tokens: 4096, structured_output: false } }),
  );
  await page.route("**/api/v1/models/active-roles", (r) =>
    r.fulfill({ json: { chat: null, tool: null, asr: null, tts: null, embedding: null } }),
  );
  await page.route("**/api/v1/models/scan", (r) => r.fulfill({ json: { found: 0 } }));
  await page.route("**/api/v1/models", (r) =>
    r.fulfill({ json: { gguf: [], llamafile: [], whisper: [], tts: [], ollama: [], embedding: [] } }),
  );

  // ── Memory-specific routes (registered last — LIFO priority) ──

  await page.route("**/api/v1/memories**", (r) => {
    // Only match GET-style list requests (no path segment after memories)
    const url = r.request().url();
    const path = new URL(url).pathname;
    if (/\/memories\/?$/.test(path) || /\/memories\?/.test(url)) {
      return r.fulfill({ status: memoriesStatus, json: memoriesStatus === 200 ? memories : { error: "server error" } });
    }
    // Anything else goes to the network; deletes are caught by the later /memories/* route.
    return r.continue();
  });

  // Delete endpoint (registered after list — takes LIFO priority for /memories/{id})
  await page.route("**/api/v1/memories/*", (r) => {
    const url = r.request().url();
    const id = url.split("/").pop()?.split("?")[0] ?? "";
    opts.onDelete?.(id);
    return r.fulfill({ status: 204, body: "" });
  });
}

async function goToMemoryScreen(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  await page.getByRole("button", { name: "Settings", exact: true }).first().click();
  await page.waitForSelector(".set", { timeout: 5_000 });
  await page.getByRole("button", { name: /^Memory$/ }).first().click();
  await page.waitForTimeout(600);
}

// ─── Tests ────────────────────────────────────────────────────

test.describe("Hub — Memory sub-screen wiring", () => {
  test("loads and populates memory list", async ({ page }) => {
    await setupMemoryRoutes(page);
    await goToMemoryScreen(page);

    await expect(page.getByText("Remembered")).toBeVisible({ timeout: 5_000 });

    await expect(
      page.getByText("Prefers the house at 70° morning, 66° overnight."),
    ).toBeVisible({ timeout: 5_000 });

    await expect(
      page.getByText("Is a software engineer; works from the office most weekdays."),
    ).toBeVisible({ timeout: 3_000 });

    await expect(
      page.getByText("Lactose intolerant — avoid dairy in recipe suggestions."),
    ).toBeVisible({ timeout: 3_000 });
  });

  test("delete button calls deleteMemory and removes row", async ({ page }) => {
    let deletedId = "";

    await setupMemoryRoutes(page, {
      onDelete: (id) => { deletedId = id; },
    });
    await goToMemoryScreen(page);

    await expect(
      page.getByText("Prefers the house at 70° morning, 66° overnight."),
    ).toBeVisible({ timeout: 5_000 });

    const firstForget = page.getByRole("button", {
      name: /forget: prefers the house/i,
    }).first();
    await expect(firstForget).toBeVisible({ timeout: 3_000 });
    await firstForget.click();

    await expect(
      page.getByText("Prefers the house at 70° morning, 66° overnight."),
    ).not.toBeVisible({ timeout: 3_000 });

    expect(deletedId).toBe("mem-1");
  });

  test("shows offline banner and mock fallback when API is unavailable", async ({ page }) => {
    await setupMemoryRoutes(page, { memoriesStatus: 500 });
    await goToMemoryScreen(page);

    await expect(
      page.getByText(/could not reach the server/i),
    ).toBeVisible({ timeout: 5_000 });

    await expect(page.getByText("Remembered")).toBeVisible({ timeout: 3_000 });
  });
});
