/**
 * Phase 8 wave 2 — Prompts sub-screen real-data wiring
 *
 * Verifies:
 * 1. Prompts screen loads from Settings > Prompts and renders preset chips
 * 2. Clicking a preset chip fetches the prompt body and populates the textarea
 * 3. Save button calls api.updatePrompt; Reset button calls api.resetPrompt
 */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { navigateTo } from "./helpers/nav";

const MOCK_PROMPTS = [
  { name: "balanced",  content: "Balanced prompt body.",  is_system: true },
  { name: "concise",   content: "Concise prompt body.",   is_system: true },
  { name: "warm",      content: "Warm prompt body.",      is_system: true },
  { name: "technical", content: "Technical prompt body.", is_system: true },
];

const MOCK_SETTINGS = {
  assistant_name: "Pond",
  user_name: "Jerry",
  chat_provider: "llamafile",
  chat_model: "llama3.2",
  agent_memory_inject: false,
  prompt_style: "balanced",
  llm_temperature: 0.7,
  llm_max_tokens: 1024,
};

async function setupPromptsRoutes(
  page: Page,
  opts: {
    onUpdate?: (name: string, content: string) => void;
    onReset?: (name: string) => void;
    prompts?: typeof MOCK_PROMPTS;
    promptStyle?: string;
  } = {},
) {
  const promptList = opts.prompts ?? MOCK_PROMPTS;
  const settings = { ...MOCK_SETTINGS, prompt_style: opts.promptStyle ?? "balanced" };

  // ── Core bootstrap routes ─────────────────────────────────
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
  await page.route("**/api/v1/settings", (r) => r.fulfill({ json: settings }));
  await page.route("**/api/v1/schedules", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/sessions", (r) => r.fulfill({ json: { sessions: [] } }));
  await page.route("**/api/v1/sessions/*/messages", (r) => r.fulfill({ json: { messages: [] } }));
  await page.route("**/api/v1/devices", (r) => r.fulfill({ json: { devices: [] } }));
  await page.route("**/api/v1/memories", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/skills", (r) => r.fulfill({ json: [] }));
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
  await page.route("**/api/v1/models/active-roles", (r) =>
    r.fulfill({ json: { chat: null, tool: null, asr: null, tts: null, embedding: null } }),
  );
  await page.route("**/api/v1/models", (r) => r.fulfill({ json: { gguf: [], llamafile: [], whisper: [], tts: [], ollama: [], embedding: [] } }));
  await page.route("**/api/v1/models/**", (r) => r.fulfill({ json: {} }));

  // ── Prompts routes — registered last for LIFO priority ───
  // Reset endpoint
  await page.route("**/api/v1/prompts/*/reset", async (r) => {
    const url = r.request().url();
    const name = url.split("/prompts/")[1]?.split("/reset")[0] ?? "balanced";
    opts.onReset?.(name);
    const original = promptList.find((p) => p.name === name) ?? promptList[0];
    return r.fulfill({ json: { ...original, content: original.content + " (reset)" } });
  });

  // Update endpoint (PUT)
  await page.route("**/api/v1/prompts/*", async (r) => {
    if (r.request().method() === "PUT") {
      const url = r.request().url();
      const name = url.split("/prompts/")[1] ?? "balanced";
      let body: { content?: string } = {};
      try { body = JSON.parse(r.request().postData() ?? "{}") as { content?: string }; } catch { /* ignore */ }
      opts.onUpdate?.(name, body.content ?? "");
      return r.fulfill({ json: { name, content: body.content ?? "", is_system: true } });
    }
    // GET single prompt
    const url = r.request().url();
    const name = url.split("/prompts/")[1] ?? "balanced";
    const found = promptList.find((p) => p.name === name) ?? promptList[0];
    return r.fulfill({ json: found });
  });

  // List prompts
  await page.route("**/api/v1/prompts", (r) => r.fulfill({ json: promptList }));
}

async function goToPromptsScreen(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  // The rail is a drawer now: Prompts is a chip under "Manage", and in the hub
  // it resolves to the Prompts detail screen directly, without the Settings list.
  await navigateTo(page, "Prompts");
  await page.waitForTimeout(600);
}

test.describe("Hub — Prompts sub-screen wiring", () => {
  test("loads preset chips from API and shows active preset body", async ({ page }) => {
    await setupPromptsRoutes(page, { promptStyle: "balanced" });
    await goToPromptsScreen(page);

    // Preset chips should be visible — use role=button to avoid matching textarea body
    await expect(page.getByRole("button", { name: "Balanced" })).toBeVisible({ timeout: 5_000 });
    await expect(page.getByRole("button", { name: "Concise" })).toBeVisible({ timeout: 3_000 });
    await expect(page.getByRole("button", { name: "Warm" })).toBeVisible({ timeout: 3_000 });
    await expect(page.getByRole("button", { name: "Technical" })).toBeVisible({ timeout: 3_000 });

    // Textarea should show the balanced body
    const ta = page.locator("textarea.prompt-ta");
    await expect(ta).toBeVisible({ timeout: 3_000 });
    await expect(ta).toHaveValue(/Balanced prompt body\./);
  });

  test("clicking a preset chip fetches and populates the textarea", async ({ page }) => {
    let fetchedPromptName: string | null = null;

    await setupPromptsRoutes(page, {
      promptStyle: "balanced",
    });

    // Override GET single prompt to track which name was requested
    await page.route("**/api/v1/prompts/*", async (r) => {
      if (r.request().method() === "GET") {
        const url = r.request().url();
        // Skip reset and list routes
        if (url.includes("/reset")) { await r.continue(); return; }
        const name = url.split("/prompts/")[1] ?? "";
        if (!name) { await r.continue(); return; }
        fetchedPromptName = name;
        const found = MOCK_PROMPTS.find((p) => p.name === name) ?? MOCK_PROMPTS[0];
        return r.fulfill({ json: found });
      }
      await r.continue();
    });

    await goToPromptsScreen(page);

    // Wait for chips to appear
    await expect(page.getByRole("button", { name: "Warm" })).toBeVisible({ timeout: 5_000 });

    // Click the "Warm" chip
    await page.getByRole("button", { name: "Warm" }).click();
    await page.waitForTimeout(500);

    // The textarea should now show warm content
    const ta = page.locator("textarea.prompt-ta");
    await expect(ta).toHaveValue(/Warm prompt body\./, { timeout: 3_000 });
  });

  test("Save calls updatePrompt and Reset calls resetPrompt", async ({ page }) => {
    let updateCalled = false;
    let resetCalled = false;

    await setupPromptsRoutes(page, {
      onUpdate: () => { updateCalled = true; },
      onReset:  () => { resetCalled  = true; },
      promptStyle: "balanced",
    });

    await goToPromptsScreen(page);

    // Wait for textarea
    const ta = page.locator("textarea.prompt-ta");
    await expect(ta).toBeVisible({ timeout: 5_000 });

    // Dirty the textarea so Save is enabled
    await ta.click();
    await ta.press("End");
    await ta.type(" edited");
    await page.waitForTimeout(200);

    // Save should now be enabled; click it
    const saveBtn = page.getByRole("button", { name: /save prompt/i });
    await expect(saveBtn).toBeEnabled({ timeout: 2_000 });
    await saveBtn.click();
    await page.waitForTimeout(500);

    expect(updateCalled).toBe(true);

    // Now click Reset
    const resetBtn = page.getByRole("button", { name: /reset prompt to default/i });
    await expect(resetBtn).toBeEnabled({ timeout: 2_000 });
    await resetBtn.click();
    await page.waitForTimeout(500);

    expect(resetCalled).toBe(true);
  });
});
