/**
 * Phase 8 wave 2 — Extensions sub-screen real-data wiring
 *
 * Verifies:
 * 1. Extensions screen loads and shows populated rows from mocked api.listExtensions()
 * 2. Toggle switch calls api.toggleExtension (PATCH) and shows flash feedback
 * 3. Browse button opens marketplace modal with cards from api.listMarketplace()
 */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { navigateTo } from "./helpers/nav";

const MOCK_EXTENSIONS = [
  {
    name: "giap-weather",
    kind: "builtin",
    description: "Local & forecast weather",
    tools: ["get_current_weather", "get_forecast"],
    enabled: true,
    status: "connected",
    last_error: null,
  },
  {
    name: "giap-news",
    kind: "builtin",
    description: "Daily headlines",
    tools: ["get_top_stories", "search_news"],
    enabled: false,
    status: undefined,
    last_error: null,
  },
  {
    name: "my-stdio-ext",
    kind: "stdio",
    description: "Custom stdio extension",
    tools: ["run_task"],
    enabled: true,
    status: "connected",
    last_error: null,
  },
];

const MOCK_MARKETPLACE = [
  {
    id: "weather-tools",
    name: "Weather Tools",
    description: "Real-time weather data and forecasts for any location worldwide.",
    kind: "streamable_http",
    uri: "http://localhost:3010/mcp",
    category: "productivity",
    author: "Pond Team",
    tools: ["get_weather", "get_forecast", "get_alerts"],
    featured: true,
    required_secrets: [],
  },
  {
    id: "git-helper",
    name: "Git Helper",
    description: "Git operations from the agent.",
    kind: "stdio",
    command: "git-mcp",
    args: [],
    category: "development",
    author: "Community",
    tools: ["git_status", "git_diff"],
    featured: false,
    required_secrets: [],
  },
];

/** Set up all routes needed for the Extensions screen tests. */
async function setupExtensionsRoutes(
  page: Page,
  opts: {
    extensions?: object[];
    onToggle?: (name: string, enabled: boolean) => void;
  } = {},
) {
  const extensionsList = opts.extensions ?? MOCK_EXTENSIONS;

  // Health + handshake
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
  await page.route("**/api/v1/models", (r) => r.fulfill({ json: { gguf: [], llamafile: [], whisper: [], tts: [], ollama: [], embedding: [] } }));
  await page.route("**/api/v1/models/active-roles", (r) => r.fulfill({ json: { chat: null, tool: null, asr: null, tts: null, embedding: null } }));
  await page.route("**/api/v1/models/**", (r) => r.fulfill({ json: {} }));
  await page.route("**/api/v1/secrets/**", (r) => r.fulfill({ json: { keys: [] } }));
  await page.route("**/api/v1/logs", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/usage", (r) => r.fulfill({ json: { total_tokens: 0, session_count: 0 } }));
  await page.route("**/api/v1/oauth/**", (r) => r.fulfill({ json: {} }));

  // Extensions-specific routes — registered last (LIFO priority)
  await page.route("**/api/v1/marketplace/*/install", (r) =>
    r.fulfill({
      status: 201,
      json: { name: "Weather Tools", kind: "streamable_http", enabled: true, tools: ["get_weather"], description: "Weather", status: "connected", last_error: null },
    }),
  );
  await page.route("**/api/v1/marketplace", (r) =>
    r.fulfill({ json: { extensions: MOCK_MARKETPLACE } }),
  );
  await page.route("**/api/v1/extensions/*/secrets", (r) =>
    r.fulfill({ json: { requirements: [], fulfilled: {} } }),
  );
  await page.route("**/api/v1/extensions/**", (r) => {
    if (r.request().method() === "PATCH") {
      const url = r.request().url();
      const nameMatch = url.match(/\/extensions\/([^/]+)$/);
      const name = nameMatch ? decodeURIComponent(nameMatch[1]) : "unknown";
      const body = r.request().postDataJSON() as { enabled: boolean };
      opts.onToggle?.(name, body.enabled);
      return r.fulfill({ status: 200, json: { status: "ok" } });
    }
    return r.fulfill({ json: { status: "ok" } });
  });
  // Extensions list — registered after the wildcard so it wins for exact path
  await page.route("**/api/v1/extensions", (r) => {
    if (r.request().method() === "POST") {
      const body = r.request().postDataJSON() as { name: string; kind: string };
      return r.fulfill({
        status: 201,
        json: { name: body.name ?? "new-ext", kind: body.kind ?? "stdio", enabled: true, tools: [], description: "", status: "connected", last_error: null },
      });
    }
    return r.fulfill({ json: { extensions: extensionsList } });
  });
}

async function goToExtensionsScreen(page: Page) {
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
  await page.getByRole("button", { name: /Extensions \(MCP\)/ }).first().click();
  await page.waitForTimeout(600);
}

test.describe("Hub — Extensions sub-screen wiring", () => {
  test("loads and shows populated extension rows", async ({ page }) => {
    await setupExtensionsRoutes(page);
    await goToExtensionsScreen(page);

    // Wait for loading to finish and rows to render
    await expect(page.getByText("giap-weather")).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText("giap-news")).toBeVisible({ timeout: 3_000 });
    await expect(page.getByText("my-stdio-ext")).toBeVisible({ timeout: 3_000 });

    // Kind badges present
    await expect(page.locator(".ext2__kind--builtin").first()).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".ext2__kind--stdio").first()).toBeVisible({ timeout: 3_000 });

    // Status dots rendered
    await expect(page.locator(".ext2__status--ok").first()).toBeVisible({ timeout: 3_000 });
  });

  test("toggle calls PATCH /api/v1/extensions/:name and shows flash", async ({ page }) => {
    let toggledName = "";
    let toggledEnabled: boolean | null = null;

    await setupExtensionsRoutes(page, {
      onToggle: (name, enabled) => {
        toggledName = name;
        toggledEnabled = enabled;
      },
    });
    await goToExtensionsScreen(page);

    // Wait for giap-news row (disabled) to appear
    await expect(page.getByText("giap-news")).toBeVisible({ timeout: 5_000 });

    // Find the toggle button for giap-news (aria-pressed=false = disabled)
    const newsToggle = page.locator(".ext2").filter({ hasText: "giap-news" }).locator("[aria-pressed]").first();
    await expect(newsToggle).toBeVisible({ timeout: 3_000 });
    await newsToggle.click();
    await page.waitForTimeout(600);

    // Verify PATCH was called
    expect(toggledName).toBe("giap-news");
    expect(toggledEnabled).toBe(true);

    // Flash message should appear
    await expect(page.locator("[role='status']")).toBeVisible({ timeout: 3_000 });
  });

  test("Browse button opens marketplace modal with cards", async ({ page }) => {
    await setupExtensionsRoutes(page);
    await goToExtensionsScreen(page);

    // Wait for screen to load
    await expect(page.getByText("giap-weather")).toBeVisible({ timeout: 5_000 });

    // Click Browse
    await page.getByRole("button", { name: /browse/i }).first().click();

    // Marketplace modal should open
    await expect(page.getByRole("dialog", { name: /marketplace/i })).toBeVisible({ timeout: 5_000 });

    // Marketplace items should load
    await expect(page.getByText("Weather Tools")).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText("Git Helper")).toBeVisible({ timeout: 3_000 });

    // Install button on non-installed item
    const installBtn = page.getByRole("button", { name: /install weather tools/i });
    await expect(installBtn).toBeVisible({ timeout: 3_000 });

    // Close modal with Escape
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog", { name: /marketplace/i })).not.toBeVisible({ timeout: 3_000 });
  });
});
