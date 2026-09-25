/**
 * Phase 8 wave 3 — Voice sub-screen real-data wiring
 *
 * Verifies:
 * 1. Voice screen loads and populates wake word + STT/TTS pickers from settings
 * 2. Speaking rate slider updates the local ttsSpeed value (debounced — no API call yet)
 * 3. TTS voice select triggers api.updateSettings({ voice_tts_voice })
 */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { navigateTo } from "./helpers/nav";

const MOCK_SETTINGS = {
  assistant_name: "Pond",
  user_name: "Jerry",
  chat_provider: "llamafile",
  chat_model: "llama3.2",
  agent_memory_inject: false,
  prompt_style: "balanced",
  voice_wake_word: "hey goose",
  voice_wake_word_transcriptions: ["hey goose", "goose"],
  active_whisper_model: "ggml-base.bin",
  voice_tts_voice: "en_US-lessac-medium.onnx",
};

/** Set up all routes needed for the Voice screen tests. */
async function setupVoiceRoutes(
  page: Page,
  opts: {
    settings?: object;
    onUpdateSettings?: (body: unknown) => void;
  } = {},
) {
  const settings = opts.settings ?? MOCK_SETTINGS;

  // ── Core bootstrap routes ──────────────────────────────────
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
  await page.route("**/api/v1/models/*/*/activate", (r) => r.fulfill({ status: 204, body: "" }));
  await page.route("**/api/v1/models", (r) => r.fulfill({ json: { gguf: [], llamafile: [], whisper: [], tts: [], ollama: [], embedding: [] } }));
  await page.route("**/api/v1/voice/calibrate", (r) => r.fulfill({ status: 204, body: "" }));

  // Settings — PUT handler (tracks updates) registered FIRST so it can intercept PATCH/PUT
  await page.route("**/api/v1/settings", async (r) => {
    if (r.request().method() === "GET") {
      return r.fulfill({ json: settings });
    }
    // PATCH / PUT
    const body = r.request().postDataJSON();
    opts.onUpdateSettings?.(body);
    return r.fulfill({ json: { ...settings, ...body } });
  });
}

async function goToVoiceScreen(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  // The hub's icon rail is gone; Settings now lives in the drawer.
  await navigateTo(page, "Settings");
  await page.waitForSelector(".set", { timeout: 5_000 });
  await page.getByRole("button", { name: /^Voice$/ }).first().click();
  await page.waitForTimeout(600);
}

test.describe("Hub — Voice sub-screen wiring", () => {
  test("loads with settings: wake word and STT model populated", async ({ page }) => {
    await setupVoiceRoutes(page);
    await goToVoiceScreen(page);

    // Card titles should be visible (use .setcard__title selector to avoid sub-text collisions)
    await expect(page.locator(".setcard__title", { hasText: "Listening" }).first()).toBeVisible({ timeout: 5_000 });
    await expect(page.locator(".setcard__title", { hasText: "Speech-to-text" }).first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".setcard__title", { hasText: "Goose's voice" }).first()).toBeVisible({ timeout: 3_000 });

    // Wake word from settings is shown
    await expect(page.getByText(/hey goose/i)).toBeVisible({ timeout: 5_000 });

    // STT picker set to the mock value (Whisper base)
    const sttSelect = page.getByRole("combobox", { name: /STT model/i });
    await expect(sttSelect).toBeVisible({ timeout: 3_000 });
    await expect(sttSelect).toHaveValue("ggml-base.bin");

    // TTS voice picker set to the mock value
    const ttsSelect = page.getByRole("combobox", { name: /TTS voice/i });
    await expect(ttsSelect).toBeVisible({ timeout: 3_000 });
    await expect(ttsSelect).toHaveValue("en_US-lessac-medium.onnx");

    // Speaking rate slider is present
    await expect(page.getByText(/Speaking rate/i)).toBeVisible({ timeout: 3_000 });
    const slider = page.locator(".hrange input[type='range']");
    await expect(slider).toBeVisible({ timeout: 3_000 });
  });

  test("TTS voice picker change calls updateSettings with correct voice", async ({ page }) => {
    let updatePayload: unknown = null;

    await setupVoiceRoutes(page, {
      onUpdateSettings: (body) => { updatePayload = body; },
    });
    await goToVoiceScreen(page);

    // Wait for TTS voice picker
    const ttsSelect = page.getByRole("combobox", { name: /TTS voice/i });
    await expect(ttsSelect).toBeVisible({ timeout: 5_000 });

    // Change to Ryan
    await ttsSelect.selectOption("en_US-ryan-medium.onnx");
    await page.waitForTimeout(500);

    // Verify updateSettings was called with the correct voice
    expect(updatePayload).toBeTruthy();
    expect((updatePayload as Record<string, unknown>).voice_tts_voice).toBe("en_US-ryan-medium.onnx");
  });

  test("speaking rate slider updates the displayed value", async ({ page }) => {
    await setupVoiceRoutes(page);
    await goToVoiceScreen(page);

    // Wait for slider
    const slider = page.locator(".hrange input[type='range']").first();
    await expect(slider).toBeVisible({ timeout: 5_000 });

    // Current value display (default 100%)
    const valDisplay = page.locator(".hrange__val").first();
    await expect(valDisplay).toContainText("100%");

    // Move slider to 120
    await slider.fill("120");
    await slider.dispatchEvent("input");
    await page.waitForTimeout(200);

    // Value display should update
    await expect(valDisplay).toContainText("120%");
  });
});
