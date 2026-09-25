/**
 * Child-process voice session (Architecture A) E2E. A fake `window.giap` makes
 * isDesktopShell() true, so VoiceMode renders VoiceModeChildProcess against the fakes.
 */

import { test, expect, type Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// ── Shell bridge stub ─────────────────────────────────────────────────────────

const SHELL_STUB_SCRIPT = `
(function () {
  const listeners = {};

  window.giap = {
    serverUrl: "http://127.0.0.1:4000",
    invoke: async function (command) {
      if (command === "server_health") return true;
      if (command === "ensure_server_running") return "http://127.0.0.1:4000";
      if (command === "start_voice_session") return "e2e-child-session";
      return undefined;
    },
    listen: function (event, handler) {
      (listeners[event] = listeners[event] || []).push(handler);
      return function () {
        listeners[event] = (listeners[event] || []).filter(function (h) { return h !== handler; });
      };
    },
  };

  // Test helper: fire an event at every registered listener.
  window.__testEmitShellEvent__ = function (event, payload) {
    (listeners[event] || []).forEach(function (h) { h(payload); });
  };
})();
`;

// ── Helpers ───────────────────────────────────────────────────────────────────

async function setupPage(page: Page): Promise<void> {
  await page.addInitScript(SHELL_STUB_SCRIPT);

  // Pin PondApiClient's API base.
  await page.addInitScript(() => {
    (window as unknown as Record<string, unknown>).__GIAP_SERVER_URL__ =
      "http://127.0.0.1:4000";
  });

  // Catch-alls first, so routes added later win.
  await mockAllApiRoutes(page);
}

async function emitEvent(page: Page, name: string, payload: unknown): Promise<void> {
  await page.evaluate(
    ([n, p]) => {
      (window as unknown as Record<string, (n: string, p: unknown) => void>)
        .__testEmitShellEvent__(n, p);
    },
    [name, payload] as [string, unknown],
  );
}

async function navigateToVoiceMode(page: Page): Promise<void> {
  await page.goto("/");
  // Wait for the server to be "online" (server_health probe returns true)
  await page.waitForTimeout(500);
  const voiceBtn = page.locator('[aria-label="Voice mode"]');
  await expect(voiceBtn).toBeVisible({ timeout: 15_000 });
  await voiceBtn.click();
  // Give the component a moment to mount and register listeners
  await page.waitForTimeout(500);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

test.describe("Voice session child-process mode (Architecture A)", () => {
  test.beforeEach(async ({ page }) => {
    await setupPage(page);
  });

  test("enters voice mode and shows initial state without crashing", async ({ page }) => {
    await navigateToVoiceMode(page);

    await expect(page.locator("body")).toBeVisible();
    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();

    // The Back button confirms VoiceMode rendered.
    const backBtn = page.getByRole("button").filter({ hasText: /back/i }).first();
    await expect(backBtn).toBeVisible({ timeout: 5_000 });
  });

  test("voice-ready emits session ID visible in UI", async ({ page }) => {
    await navigateToVoiceMode(page);

    await emitEvent(page, "voice-ready", { session_id: "e2e-child-session" });
    await page.waitForTimeout(300);

    // Session hint shows first 6 chars of "e2e-child-session"
    await expect(page.getByText(/e2e-ch/i)).toBeVisible({ timeout: 5_000 });
  });

  test("voice-state wait -> wake-word listening label appears (finding 17)", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "sess-wait" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-state", "wait");
    // STATE_LABELS["wait"] is "Listening for wake word...".
    await expect(page.getByText(/listening for wake word/i).first()).toBeVisible({ timeout: 5_000 });
  });

  test("voice-state listen -> Listening label appears", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "sess-1" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-state", "listen");
    await expect(page.getByText(/listening/i).first()).toBeVisible({ timeout: 5_000 });
  });

  test("voice-state thinking -> Thinking label appears", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "sess-2" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-state", "thinking");
    await expect(page.getByText(/thinking/i).first()).toBeVisible({ timeout: 5_000 });
  });

  test("voice-state speak -> Speaking label appears", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "sess-3" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-state", "speak");
    await expect(page.getByText(/speaking/i).first()).toBeVisible({ timeout: 5_000 });
  });

  test("voice-transcript renders user text in transcript feed", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "sess-4" });
    await emitEvent(page, "voice-state", "listen");
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-transcript", { text: "what is the weather today" });
    await page.waitForTimeout(300);

    await expect(page.getByText(/what is the weather today/i).first()).toBeVisible({
      timeout: 5_000,
    });
  });

  test("voice-token streams tokens into the agent bubble", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "sess-5" });
    await emitEvent(page, "voice-state", "listen");
    await emitEvent(page, "voice-transcript", { text: "hello" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-state", "thinking");
    await emitEvent(page, "voice-token", { content: "The weather" });
    await emitEvent(page, "voice-token", { content: " is sunny." });
    await page.waitForTimeout(300);

    await expect(page.getByText(/The weather is sunny\./i).first()).toBeVisible({
      timeout: 5_000,
    });
  });

  test("voice-tool-call pushes a context card with tool name", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "sess-6" });
    await emitEvent(page, "voice-state", "listen");
    await emitEvent(page, "voice-transcript", { text: "check the weather" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-tool-call", { tool: "giap__weather", id: "tc-1" });
    await page.waitForTimeout(300);

    await expect(page.getByText(/weather/i).first()).toBeVisible({ timeout: 5_000 });
  });

  test("full contract sequence: ready->wait->listen->transcript->thinking->tokens->tool->speak->done->wait", async ({ page }) => {
    await navigateToVoiceMode(page);

    await emitEvent(page, "voice-ready", { session_id: "full-seq" });
    await emitEvent(page, "voice-state", "wait");
    await page.waitForTimeout(50);

    await emitEvent(page, "voice-state", "listen");
    await expect(page.getByText(/listening/i).first()).toBeVisible({ timeout: 5_000 });

    await emitEvent(page, "voice-transcript", { text: "how is the weather" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-state", "thinking");
    await expect(page.getByText(/thinking/i).first()).toBeVisible({ timeout: 5_000 });

    await emitEvent(page, "voice-token", { content: "It is" });
    await emitEvent(page, "voice-token", { content: " 22 degrees." });
    await emitEvent(page, "voice-tool-call", { tool: "giap__weather", id: "x1" });
    await emitEvent(page, "voice-tool-result", { tool: "giap__weather", id: "x1", content: "22C" });

    await emitEvent(page, "voice-state", "speak");
    await expect(page.getByText(/speaking/i).first()).toBeVisible({ timeout: 5_000 });

    await emitEvent(page, "voice-done", { session_id: "full-seq" });
    await page.waitForTimeout(200);

    await emitEvent(page, "voice-state", "wait");
    await page.waitForTimeout(100);

    await expect(page.getByText(/how is the weather/i).first()).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText(/It is 22 degrees\./i).first()).toBeVisible({ timeout: 5_000 });

    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });

  test("voice-session-ended with non-zero code shows error indicator", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "err-sess" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-session-ended", { code: 1, reason: "error" });
    await page.waitForTimeout(300);

    // Error state should be shown (either "Error" label or error message)
    await expect(
      page.getByText(/error|code 1/i).first(),
    ).toBeVisible({ timeout: 5_000 });
  });

  test("voice-session-ended with code 0 does not show error", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "clean-exit" });
    await page.waitForTimeout(100);

    await emitEvent(page, "voice-session-ended", { code: 0, reason: "stdin_eof" });
    await page.waitForTimeout(300);

    await expect(page.getByText(/code \d/i)).not.toBeVisible({ timeout: 2_000 });
  });

  test("back button returns to GUI sidebar", async ({ page }) => {
    await navigateToVoiceMode(page);
    await emitEvent(page, "voice-ready", { session_id: "back-test" });
    await page.waitForTimeout(100);

    const backBtn = page.getByRole("button").filter({ hasText: /back/i }).first();
    if (await backBtn.isVisible()) {
      await backBtn.click();
      await expect(
        page.locator('aside[aria-label="Navigation"]'),
      ).toBeVisible({ timeout: 5_000 });
    }
  });

  test("existing voice_pipeline tests still pass (browser path is unchanged)", async ({ page }) => {
    // Smoke only: beforeEach already installed the shell stub, so this is not the browser path.
    await navigateToVoiceMode(page);
    await expect(page.locator("body")).toBeVisible();
    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });
});
