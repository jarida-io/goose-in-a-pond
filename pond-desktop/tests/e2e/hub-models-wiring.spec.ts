/** Models sub-screen (Settings > Models) wired to listModels and activateModel. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

const MOCK_MODELS = {
  gguf: [
    {
      name: "gemma-4-E4B-it-Q4_K_M",
      description: "Gemma 4 E4B Instruct (Q4_K_M)",
      active: false,
      downloaded: true,
      ram_estimate_mb: 3100,
      recommended_role: "chat",
      size_mb: 2560,
      category: "llm",
    },
  ],
  llamafile: [],
  whisper: [
    {
      name: "base.en",
      description: "Whisper base (English)",
      active: false,
      downloaded: true,
      size_mb: 148,
      category: "asr",
    },
  ],
  tts: [
    {
      name: "en-lessac-medium",
      description: "Piper en-US Lessac (medium)",
      active: false,
      downloaded: true,
      size_mb: 65,
      category: "tts",
      tts_engine: "piper",
    },
  ],
  ollama: [],
  embedding: [],
};

const INITIAL_ROLES = {
  chat: null,
  tool: null,
  asr: null,
  tts: null,
  embedding: null,
};

const AFTER_ACTIVATE_ROLES = {
  chat: { provider: "gguf", model: "gemma-4-E4B-it-Q4_K_M" },
  tool: null,
  asr: null,
  tts: null,
  embedding: null,
};

/** Call before navigating, so the routes are in place. */
async function setupModelsRoutes(
  page: Page,
  opts: {
    roles?: object;
    onActivate?: () => void;
  } = {},
) {
  const roles = opts.roles ?? INITIAL_ROLES;

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
    r.fulfill({ json: { assistant_name: "Pond", user_name: "Jerry", chat_provider: "llamafile", chat_model: "llama3.2", agent_memory_inject: false, prompt_style: "balanced", llm_temperature: 0.7, llm_max_tokens: 1024 } }),
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

  // Models-specific routes — registered last so they take LIFO priority
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
  // Activate endpoint — must be before the broad active-roles + models routes
  await page.route("**/api/v1/models/*/*/activate", (r) => {
    opts.onActivate?.();
    return r.fulfill({ status: 204, body: "" });
  });
  await page.route("**/api/v1/models/scan", (r) => r.fulfill({ json: { found: 0 } }));

  // Active-roles: use a mutable reference so the test can swap it
  let rolesPayload = roles;
  await page.route("**/api/v1/models/active-roles", (r) =>
    r.fulfill({ json: rolesPayload }),
  );
  (page as Page & { _setRoles: (r: object) => void })._setRoles = (r) => { rolesPayload = r; };

  await page.route("**/api/v1/models", (r) => r.fulfill({ json: MOCK_MODELS }));
}

async function goToModelsScreen(page: Page) {
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
  await page.getByRole("button", { name: /^Models$/ }).first().click();
  await page.waitForTimeout(600);
}

test.describe("Hub — Models sub-screen wiring", () => {
  test("loads model rows and shows Load button", async ({ page }) => {
    await setupModelsRoutes(page);
    await goToModelsScreen(page);

    await expect(page.getByText("Language models")).toBeVisible({ timeout: 5_000 });

    await expect(page.getByText(/gemma-4-E4B-it-Q4_K_M/i)).toBeVisible({ timeout: 5_000 });

    const loadBtn = page.getByRole("button", { name: /load gemma-4-E4B-it-Q4_K_M as chat model/i });
    await expect(loadBtn).toBeVisible({ timeout: 3_000 });
  });

  test("Load button calls activateModel and role refreshes", async ({ page }) => {
    let activateCalled = false;

    await setupModelsRoutes(page, {
      onActivate: () => { activateCalled = true; },
    });

    await page.route("**/api/v1/models/*/*/activate", async (r) => {
      activateCalled = true;
      await r.fulfill({ status: 204, body: "" });
      // Update mocked roles so the re-fetch sees the activated model
      (page as Page & { _setRoles: (r: object) => void })._setRoles(AFTER_ACTIVATE_ROLES);
    });

    await goToModelsScreen(page);

    await expect(page.getByText(/gemma-4-E4B-it-Q4_K_M/i)).toBeVisible({ timeout: 5_000 });

    await page.getByRole("button", { name: /load gemma-4-E4B-it-Q4_K_M as chat model/i }).click();
    await page.waitForTimeout(1000);

    expect(activateCalled).toBe(true);
  });

  test("speech models are listed with Use buttons", async ({ page }) => {
    await setupModelsRoutes(page);
    await goToModelsScreen(page);

    await expect(page.locator(".setcard").filter({ hasText: "Speech" }).first()).toBeVisible({ timeout: 5_000 });

    await expect(page.getByText("Whisper base (English)")).toBeVisible({ timeout: 3_000 });

    await expect(page.getByText("Piper en-US Lessac (medium)")).toBeVisible({ timeout: 3_000 });

    const useButtons = page.getByRole("button", { name: /use .+ as (speech-to-text|text-to-speech)/i });
    await expect(useButtons.first()).toBeVisible({ timeout: 3_000 });
  });
});
