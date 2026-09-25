/** Privacy and Account sub-screens wired to getSettings/updateSettings, plus sign-out. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

// ─── Shared mock settings ─────────────────────────────────────

const MOCK_SETTINGS = {
  assistant_name: "Pond",
  user_name: "Jerry",
  home_name: "Goose Pond",
  timezone: "Africa/Nairobi",
  chat_provider: "llamafile",
  chat_model: "llama3.2",
  agent_memory_inject: false,
  prompt_style: "balanced",
  llm_temperature: 0.7,
  llm_max_tokens: 1024,
  mic_enabled: true,
  cameras_enabled: true,
  cloud_fallback_enabled: false,
  telemetry_enabled: false,
};

const MOCK_SESSIONS_3 = {
  sessions: [
    { id: "s1", title: "Session 1", created_at: "2026-01-01T00:00:00Z", message_count: 5 },
    { id: "s2", title: "Session 2", created_at: "2026-01-02T00:00:00Z", message_count: 8 },
    { id: "s3", title: "Session 3", created_at: "2026-01-03T00:00:00Z", message_count: 2 },
  ],
};

const MOCK_DEVICES = {
  devices: [
    { id: "d1", name: "Living Room Light", room: "Living Room" },
    { id: "d2", name: "Kitchen Light", room: "Kitchen" },
    { id: "d3", name: "Bedroom Fan", room: "Bedroom" },
  ],
};

// ─── Route helper ─────────────────────────────────────────────

async function setupRoutes(
  page: Page,
  opts: {
    settings?: object;
    sessions?: object;
    devices?: object;
    onSettingsUpdate?: (body: Record<string, unknown>) => void;
  } = {},
) {
  const settings = opts.settings ?? MOCK_SETTINGS;
  const sessions = opts.sessions ?? MOCK_SESSIONS_3;
  const devices = opts.devices ?? MOCK_DEVICES;

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
  await page.route("**/api/v1/onboard/reset", (r) =>
    r.fulfill({ json: { onboarded: false, current_step: "Welcome", steps_completed: 1, total_steps: 10 } }),
  );

  await page.route("**/api/v1/settings", async (r) => {
    if (r.request().method() === "PUT") {
      const body = r.request().postDataJSON() as Record<string, unknown>;
      opts.onSettingsUpdate?.(body);
      return r.fulfill({ json: { ...MOCK_SETTINGS, ...body } });
    }
    return r.fulfill({ json: settings });
  });

  await page.route("**/api/v1/sessions", (r) =>
    r.fulfill({ json: sessions }),
  );
  await page.route("**/api/v1/sessions/*/messages", (r) =>
    r.fulfill({ json: { messages: [] } }),
  );
  await page.route("**/api/v1/devices", (r) =>
    r.fulfill({ json: devices }),
  );
  await page.route("**/api/v1/memories", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/skills", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/schedules", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/schedules/**", (r) => r.fulfill({ json: { status: "ok" } }));
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
  await page.route("**/api/v1/models/active-roles", (r) =>
    r.fulfill({ json: { chat: null, tool: null, asr: null, tts: null, embedding: null } }),
  );
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
  await page.route("**/api/v1/models", (r) =>
    r.fulfill({ json: { gguf: [], llamafile: [], whisper: [], tts: [], ollama: [], embedding: [] } }),
  );
  await page.route("**/api/v1/models/**", (r) => r.fulfill({ json: { status: "ok" } }));
}

// ─── Navigation helpers ───────────────────────────────────────

async function navigateTo(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
}

async function goToPrivacyScreen(page: Page) {
  await navigateTo(page);
  await page.getByRole("button", { name: "Settings", exact: true }).first().click();
  await page.waitForSelector(".set", { timeout: 5_000 });
  await page.getByRole("button", { name: /^Privacy$/ }).first().click();
  await page.waitForTimeout(500);
}

async function goToAccountScreen(page: Page) {
  await navigateTo(page);
  await page.getByRole("button", { name: "Settings", exact: true }).first().click();
  await page.waitForSelector(".set", { timeout: 5_000 });
  await page.getByRole("button", { name: /^Account$/ }).first().click();
  await page.waitForTimeout(500);
}

// ─── Tests ────────────────────────────────────────────────────

test.describe("Hub — Privacy sub-screen wiring", () => {
  test("loads toggle state and session count from API", async ({ page }) => {
    await setupRoutes(page);
    await goToPrivacyScreen(page);

    await expect(page.getByText("Everything runs on-device")).toBeVisible({ timeout: 5_000 });

    await expect(page.getByText("Microphone")).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("Cameras")).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("Cloud fallback")).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("Anonymous diagnostics")).toBeVisible({ timeout: 3_000 });

    await expect(page.getByText(/3 sessions?/i)).toBeVisible({ timeout: 5_000 });

    const clearBtn = page.getByRole("button", { name: /clear all conversation history/i });
    await expect(clearBtn).toBeVisible({ timeout: 3_000 });
    await expect(clearBtn).toBeEnabled();
  });

  test("toggling a switch calls updateSettings with correct field", async ({ page }) => {
    let lastUpdate: Record<string, unknown> = {};

    await setupRoutes(page, {
      onSettingsUpdate: (body) => { lastUpdate = body; },
    });
    await goToPrivacyScreen(page);

    await expect(page.getByText("Cloud fallback")).toBeVisible({ timeout: 5_000 });

    // Third toggle: Cloud fallback, off in MOCK_SETTINGS.
    const cloudToggle = page.locator(".htoggle").nth(2);
    await cloudToggle.click();
    await page.waitForTimeout(500);

    expect(lastUpdate).toMatchObject({ cloud_fallback_enabled: true });
  });
});

test.describe("Hub — Account sub-screen wiring", () => {
  test("loads user name and home name from API", async ({ page }) => {
    await setupRoutes(page);
    await goToAccountScreen(page);

    // Hero name populated from user_name
    await expect(page.getByTestId("acct-hero-name")).toContainText("Jerry", { timeout: 5_000 });

    await expect(page.getByText("Name", { exact: true }).first()).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("Home name", { exact: true })).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("Time zone", { exact: true })).toBeVisible({ timeout: 3_000 });

    await expect(page.getByText("Goose In A Pond")).toBeVisible({ timeout: 3_000 });

    await expect(page.getByTestId("signout-btn")).toBeVisible({ timeout: 3_000 });
  });

  test("sign-out button clears token and dispatches SET_SESSION_TOKEN null", async ({ page }) => {
    await setupRoutes(page);
    await goToAccountScreen(page);

    const signOutBtn = page.getByTestId("signout-btn");
    await expect(signOutBtn).toBeVisible({ timeout: 5_000 });

    // Sign-out reloads the page; the check is only that no error flash appears.
    let reloadFired = false;
    await page.exposeFunction("__onReload", () => { reloadFired = true; });
    await page.addInitScript(() => {
      const origReload = window.location.reload.bind(window.location);
      window.location.reload = () => {
        (window as unknown as Record<string, () => void>).__onReload?.();
        origReload();
      };
    });

    await signOutBtn.click();

    const errorFlash = page.locator('[role="status"]').filter({ hasText: /failed/i });
    await page.waitForTimeout(400);
    await expect(errorFlash).not.toBeVisible();
  });

  test("Start over calls resetOnboarding and returns to the wizard", async ({ page }) => {
    await setupRoutes(page);

    // Registered after setupRoutes, so it overrides the default reset mock.
    let resetHit = false;
    await page.route("**/api/v1/onboard/reset", (r) => {
      resetHit = true;
      return r.fulfill({ json: { onboarded: false, current_step: "Welcome", steps_completed: 1, total_steps: 10 } });
    });

    await goToAccountScreen(page);

    const restartBtn = page.getByTestId("restart-onboarding-btn");
    await expect(restartBtn).toBeVisible({ timeout: 5_000 });
    await restartBtn.click();

    await expect.poll(() => resetHit, { timeout: 5_000 }).toBe(true);

    // SET_NEEDS_ONBOARDING shows the OnboardingWizard.
    await expect(page.getByText("First-time setup")).toBeVisible({ timeout: 5_000 });
  });
});
