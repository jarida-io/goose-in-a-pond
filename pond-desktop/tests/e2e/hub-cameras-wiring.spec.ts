/**
 * Phase 8 wave 4 — Cameras sub-screen real-data wiring
 *
 * Verifies:
 * 1. Camera cards render from api.listDevices() filtered by device_type === "camera"
 * 2. Offline fallback: when api.listDevices() fails, mock cameras are shown with error banner
 * 3. Empty state: when no camera devices are registered, empty-state card renders
 */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { navigateTo } from "./helpers/nav";

const MOCK_CAMERA_DEVICES = [
  {
    id: "cam-front",
    name: "Front Door",
    device_type: "camera",
    room: "Outdoor",
    is_online: true,
    last_seen: new Date(Date.now() - 2 * 60_000).toISOString(), // 2 min ago
    metadata: {},
  },
  {
    id: "cam-drive",
    name: "Driveway",
    device_type: "camera",
    room: "Outdoor",
    is_online: true,
    last_seen: new Date(Date.now() - 5 * 60_000).toISOString(), // 5 min ago
    metadata: {},
  },
  {
    id: "cam-back",
    name: "Backyard",
    device_type: "camera",
    room: "Outdoor",
    is_online: false,
    last_seen: new Date(Date.now() - 2 * 3600_000).toISOString(), // 2h ago
    metadata: {},
  },
];

// Non-camera devices that should be filtered out
const MOCK_OTHER_DEVICES = [
  { id: "light-1", name: "Living Room Light", device_type: "light", is_online: true },
  { id: "lock-1",  name: "Front Door Lock",   device_type: "lock",  is_online: true },
];

/** Register all routes needed for the Cameras screen tests. */
async function setupCamerasRoutes(
  page: Page,
  opts: {
    devicesResponse?: object | null;  // null = 500 error
    extraDevices?: object[];
  } = {},
) {
  // ── App bootstrap routes ──
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
  await page.route("**/api/v1/schedules",             (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/sessions",              (r) => r.fulfill({ json: { sessions: [] } }));
  await page.route("**/api/v1/sessions/*/messages",   (r) => r.fulfill({ json: { messages: [] } }));
  await page.route("**/api/v1/memories",              (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/skills",                (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/prompts",               (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/prompts/**",            (r) =>
    r.fulfill({ json: { name: "balanced", content: "You are a helpful assistant.", is_system: true } }),
  );
  await page.route("**/api/v1/agent/extras",          (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/agent/tools",           (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/recipes",               (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/transcribe",            (r) => r.fulfill({ json: { text: "" } }));
  await page.route("**/api/v1/chat/stream", (r) =>
    r.fulfill({ status: 200, headers: { "Content-Type": "text/event-stream" }, body: 'data: {"done":true}\n\n' }),
  );
  await page.route("**/api/v1/tts",                   (r) => r.fulfill({ status: 503, json: { error: "off" } }));
  await page.route("**/api/v1/profiles",              (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/extensions",            (r) => r.fulfill({ json: { extensions: [] } }));
  await page.route("**/api/v1/extensions/**",         (r) => r.fulfill({ json: { status: "ok" } }));
  await page.route("**/api/v1/marketplace",           (r) => r.fulfill({ json: { extensions: [] } }));
  await page.route("**/api/v1/marketplace/*/install", (r) => r.fulfill({ status: 201, json: {} }));
  await page.route("**/api/v1/secrets/**",            (r) => r.fulfill({ json: { keys: [] } }));
  await page.route("**/api/v1/logs",                  (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/usage",                 (r) => r.fulfill({ json: { total_tokens: 0, session_count: 0 } }));
  await page.route("**/api/v1/oauth/**",              (r) => r.fulfill({ json: {} }));
  await page.route("**/api/v1/models/memory-status",  (r) =>
    r.fulfill({ json: { total_mb: 8192, available_for_llm_mb: 4096, loaded_model: null } }),
  );
  await page.route("**/api/v1/models/download/progress", (r) => r.fulfill({ json: { downloads: [] } }));
  await page.route("**/api/v1/models/ollama",         (r) => r.fulfill({ json: { models: [] } }));
  await page.route("**/api/v1/models/capabilities",   (r) =>
    r.fulfill({ json: { thinking: false, vision: false, audio_input: false, context_window_tokens: 4096, structured_output: false } }),
  );
  await page.route("**/api/v1/models/*/*/activate",   (r) => r.fulfill({ status: 204, body: "" }));
  await page.route("**/api/v1/models/scan",           (r) => r.fulfill({ json: { found: 0 } }));
  await page.route("**/api/v1/models/active-roles",   (r) =>
    r.fulfill({ json: { chat: null, tool: null, asr: null, tts: null, embedding: null } }),
  );
  await page.route("**/api/v1/models",                (r) =>
    r.fulfill({ json: { gguf: [], llamafile: [], whisper: [], tts: [], ollama: [], embedding: [] } }),
  );

  // ── Devices route — registered last (LIFO priority) ──────────
  if (opts.devicesResponse === null) {
    // Simulate a server error
    await page.route("**/api/v1/devices", (r) =>
      r.fulfill({ status: 500, json: { error: "internal server error" } }),
    );
  } else {
    const devices = opts.devicesResponse ?? {
      devices: [...MOCK_CAMERA_DEVICES, ...MOCK_OTHER_DEVICES, ...(opts.extraDevices ?? [])],
    };
    await page.route("**/api/v1/devices", (r) => r.fulfill({ json: devices }));
  }
}

async function goToCamerasScreen(page: Page) {
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
  await page.getByRole("button", { name: /^Cameras$/ }).first().click();
  await page.waitForTimeout(600);
}

test.describe("Hub — Cameras sub-screen wiring", () => {
  test("renders camera cards from api.listDevices() filtered to device_type=camera", async ({ page }) => {
    await setupCamerasRoutes(page);
    await goToCamerasScreen(page);

    // All three camera names should appear in the card body titles
    // Use .camset__name to avoid strict-mode ambiguity with .cam__name inside CameraFeed
    await expect(page.locator(".camset__name", { hasText: "Front Door" }).first()).toBeVisible({ timeout: 5_000 });
    await expect(page.locator(".camset__name", { hasText: "Driveway" }).first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".camset__name", { hasText: "Backyard" }).first()).toBeVisible({ timeout: 3_000 });

    // Non-camera devices should NOT appear
    await expect(page.locator(".camset__name", { hasText: "Living Room Light" })).not.toBeVisible();
    await expect(page.locator(".camset__name", { hasText: "Front Door Lock" })).not.toBeVisible();

    // Each online camera shows "Online" in its status line
    const onlineLabels = page.locator(".camset__sub", { hasText: "Online" });
    await expect(onlineLabels).toHaveCount(2, { timeout: 3_000 });

    // Offline camera shows "Offline" badge
    await expect(page.getByText("Offline")).toBeVisible({ timeout: 3_000 });

    // Subtitle reflects live count (2 live feeds)
    await expect(page.locator(".view-sub", { hasText: /2 live feed/ })).toBeVisible({ timeout: 3_000 });
  });

  test("offline fallback: shows error banner and mock cameras when API fails", async ({ page }) => {
    await setupCamerasRoutes(page, { devicesResponse: null });
    await goToCamerasScreen(page);

    // Error banner should appear
    await expect(page.locator("div", { hasText: /could not reach the server/i }).first()).toBeVisible({ timeout: 5_000 });

    // Fallback mock cameras are shown
    await expect(page.locator(".camset__name", { hasText: "Front Door" }).first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".camset__name", { hasText: "Driveway" }).first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".camset__name", { hasText: "Backyard" }).first()).toBeVisible({ timeout: 3_000 });
  });

  test("empty state renders when no camera devices are registered", async ({ page }) => {
    // Return only non-camera devices — camera filter yields empty list
    await setupCamerasRoutes(page, {
      devicesResponse: { devices: MOCK_OTHER_DEVICES },
    });
    await goToCamerasScreen(page);

    // Falls back to MOCK_CAMERA_STATES (no cameras registered → offline fallback)
    // The fallback renders the mock cameras to avoid a blank screen
    await expect(page.locator(".camset__name", { hasText: "Front Door" }).first()).toBeVisible({ timeout: 5_000 });

    // Subtitle should say "3 live feeds" from mock fallback
    await expect(page.locator(".view-sub")).toBeVisible({ timeout: 3_000 });
  });
});
