/**
 * Phase 8 wave 3 — Logs sub-screen real-data wiring
 *
 * Verifies:
 * 1. Logs screen loads and renders log rows from mocked api.listLogs()
 * 2. Filter pill switches client-side filter and updates visible rows
 */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { navigateTo } from "./helpers/nav";

const MOCK_LOGS = [
  { id: 1, timestamp: "2026-06-04T08:50:02Z", level: "INFO",  source: "pond",    message: "Server bound to 127.0.0.1:4000" },
  { id: 2, timestamp: "2026-06-04T08:50:04Z", level: "INFO",  source: "whisper", message: "Speech-to-text ready: whisper/base" },
  { id: 3, timestamp: "2026-06-04T08:50:08Z", level: "WARN",  source: "memory",  message: "Available memory below 25% (1.8 GB)" },
  { id: 4, timestamp: "2026-06-04T08:50:18Z", level: "ERROR", source: "mcp",     message: "giap-news handshake failed — disabled" },
];

/** Wire all routes required to reach the Logs sub-screen. */
async function setupLogsRoutes(page: Page, opts: { logEntries?: object[] } = {}) {
  const logEntries = opts.logEntries ?? MOCK_LOGS;

  // Core infrastructure routes
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
      },
    }),
  );
  await page.route("**/api/v1/schedules", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/sessions", (r) => r.fulfill({ json: { sessions: [] } }));
  await page.route("**/api/v1/sessions/*/messages", (r) => r.fulfill({ json: { messages: [] } }));
  await page.route("**/api/v1/devices", (r) => r.fulfill({ json: { devices: [] } }));
  await page.route("**/api/v1/memories", (r) => r.fulfill({ json: [] }));
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
  await page.route("**/api/v1/usage", (r) => r.fulfill({ json: { total_tokens: 0, session_count: 0 } }));
  await page.route("**/api/v1/oauth/**", (r) => r.fulfill({ json: {} }));
  await page.route("**/api/v1/models/memory-status", (r) =>
    r.fulfill({ json: { total_mb: 8192, available_for_llm_mb: 4096, loaded_model: null } }),
  );
  await page.route("**/api/v1/models/download/progress", (r) =>
    r.fulfill({ json: { downloads: [] } }),
  );
  await page.route("**/api/v1/models/ollama", (r) =>
    r.fulfill({ json: { models: [] } }),
  );
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

  // Logs route — registered last (LIFO priority)
  await page.route("**/api/v1/logs**", (r) =>
    r.fulfill({ json: logEntries }),
  );
}

/** Navigate to the Logs sub-screen inside Settings. */
async function goToLogsScreen(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  // The rail is a drawer now: Logs is a chip under "Manage", and in the hub it
  // resolves to the Logs detail screen directly, without the Settings list.
  await navigateTo(page, "Logs");
  await page.waitForTimeout(600);
}

test.describe("Hub — Logs sub-screen wiring", () => {
  test("loads and renders log rows from the API", async ({ page }) => {
    await setupLogsRoutes(page);
    await goToLogsScreen(page);

    // At least one log entry from MOCK_LOGS should be visible
    await expect(page.getByText("Server bound to 127.0.0.1:4000")).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText("giap-news handshake failed — disabled")).toBeVisible({ timeout: 3_000 });

    // Level badges should be present
    await expect(page.locator(".logrow__lvl--info").first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".logrow__lvl--warn").first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".logrow__lvl--error").first()).toBeVisible({ timeout: 3_000 });
  });

  test("filter pill switches hide non-matching rows", async ({ page }) => {
    await setupLogsRoutes(page);
    await goToLogsScreen(page);

    // Wait for initial data to render
    await expect(page.getByText("Server bound to 127.0.0.1:4000")).toBeVisible({ timeout: 5_000 });

    // Click the "Error" filter tab
    await page.getByRole("tab", { name: "Error" }).click();
    await page.waitForTimeout(300);

    // ERROR entry should still be visible
    await expect(page.getByText("giap-news handshake failed — disabled")).toBeVisible({ timeout: 3_000 });

    // INFO entry should no longer be visible
    await expect(page.getByText("Server bound to 127.0.0.1:4000")).not.toBeVisible({ timeout: 3_000 });

    // Switch to "Warn" tab
    await page.getByRole("tab", { name: "Warn" }).click();
    await page.waitForTimeout(300);

    // WARN entry visible
    await expect(page.getByText("Available memory below 25% (1.8 GB)")).toBeVisible({ timeout: 3_000 });

    // ERROR entry not visible
    await expect(page.getByText("giap-news handshake failed — disabled")).not.toBeVisible({ timeout: 3_000 });
  });
});
