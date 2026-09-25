/** Rooms sub-screen (Settings > Rooms): devices grouped by room, toggles via tools/invoke. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

// ─── Mock data ──────────────────────────────────────────────────
const MOCK_DEVICES = {
  devices: [
    { id: "dev-1", name: "Jetson Orin Nano",  device_type: "host",   room: "Office",      is_online: true,  last_seen: new Date().toISOString() },
    { id: "dev-2", name: "Desk Sensor",        device_type: "sensor", room: "Office",      is_online: false, last_seen: new Date(Date.now() - 3_600_000).toISOString() },
    { id: "dev-3", name: "Living Room Hub",    device_type: "host",   room: "Living Room", is_online: true,  last_seen: new Date().toISOString() },
    { id: "dev-4", name: "Motion Sensor",      device_type: "sensor", room: "Living Room", is_online: true,  last_seen: new Date().toISOString() },
    { id: "dev-5", name: "Front Door Camera",  device_type: "sensor", room: "Outdoor",     is_online: true,  last_seen: new Date().toISOString() },
  ],
};

// ─── Route helpers ───────────────────────────────────────────────
async function setupBaseRoutes(page: Page) {
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
  await page.route("**/api/v1/models/active-roles", (r) =>
    r.fulfill({ json: { chat: null, tool: null, asr: null, tts: null, embedding: null } }),
  );
  await page.route("**/api/v1/models/memory-status", (r) =>
    r.fulfill({ json: { total_mb: 8192, available_for_llm_mb: 4096, loaded_model: null } }),
  );
  await page.route("**/api/v1/models/download/progress", (r) => r.fulfill({ json: { downloads: [] } }));
  await page.route("**/api/v1/models/ollama", (r) => r.fulfill({ json: { models: [] } }));
  await page.route("**/api/v1/models/capabilities", (r) =>
    r.fulfill({ json: { thinking: false, vision: false, audio_input: false, context_window_tokens: 4096, structured_output: false } }),
  );
  await page.route("**/api/v1/models/*/*/activate", (r) => r.fulfill({ status: 204, body: "" }));
  await page.route("**/api/v1/models/scan", (r) => r.fulfill({ json: { found: 0 } }));
  await page.route("**/api/v1/models", (r) => r.fulfill({ json: { gguf: [], llamafile: [], whisper: [], tts: [], ollama: [], embedding: [] } }));
}

async function goToRoomsScreen(page: Page) {
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
  await page.getByRole("button", { name: /^Rooms/ }).first().click();
  await page.waitForTimeout(600);
}

// ─── Tests ───────────────────────────────────────────────────────
test.describe("Hub — Rooms sub-screen wiring", () => {
  test("loads devices and groups them by room with device count badge", async ({ page }) => {
    await setupBaseRoutes(page);
    await page.route("**/api/v1/devices", (r) => r.fulfill({ json: MOCK_DEVICES }));

    await goToRoomsScreen(page);

    await expect(page.getByText("Jetson Orin Nano")).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText("Living Room Hub")).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("Front Door Camera")).toBeVisible({ timeout: 3_000 });

    // Device count badges — Office has 2, Living Room has 2, Outdoor has 1
    const countBadges = page.locator(".room-count");
    await expect(countBadges.first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".room-count").filter({ hasText: "2 devices" }).first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".room-count").filter({ hasText: "1 device" })).toBeVisible({ timeout: 3_000 });

    await expect(page.getByText("Jetson Orin Nano")).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("Living Room Hub")).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("Front Door Camera")).toBeVisible({ timeout: 3_000 });
  });

  test("toggle calls set_device_state via the device-control MCP tool", async ({ page }) => {
    let toolCallPayload: Record<string, unknown> | null = null;

    await setupBaseRoutes(page);
    await page.route("**/api/v1/devices", (r) => r.fulfill({ json: MOCK_DEVICES }));
    await page.route("**/api/v1/tools/invoke", async (r) => {
      const body = (await r.request().postDataJSON()) as Record<string, unknown>;
      toolCallPayload = body;
      return r.fulfill({
        json: { tool: "giap-device-control__set_device_state", success: true, content: "ok" },
      });
    });

    await goToRoomsScreen(page);

    await expect(page.getByText("Jetson Orin Nano")).toBeVisible({ timeout: 5_000 });

    const toggles = page.locator(".htoggle");
    await toggles.first().click();
    await page.waitForTimeout(500);

    expect(toolCallPayload).not.toBeNull();
    expect(toolCallPayload?.server).toBe("giap-device-control");
    expect(toolCallPayload?.tool).toBe("set_device_state");
    expect(toolCallPayload?.args).toBeDefined();
  });

  test("shows offline mock fallback when API returns error", async ({ page }) => {
    await setupBaseRoutes(page);
    await page.route("**/api/v1/devices", (r) =>
      r.fulfill({ status: 500, json: { error: "server error" } }),
    );

    await goToRoomsScreen(page);

    await expect(page.getByText(/could not reach the server/i)).toBeVisible({ timeout: 5_000 });

    await expect(page.locator(".setcard").first()).toBeVisible({ timeout: 3_000 });
  });
});
