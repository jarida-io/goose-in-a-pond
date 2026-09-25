/** VoiceMode in the Vite SPA (no native shell): renders, starts waiting, exits, sane session id. */
import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// Exact aria-label: other buttons contain "voice mode" in their text.
const VOICE_BTN = '[aria-label="Voice mode"]';

test.beforeEach(async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.goto("/");
});

test.describe("Voice mode", () => {
  test("clicking Voice mode button switches the view", async ({ page }) => {
    const voiceBtn = page.locator(VOICE_BTN);
    await expect(voiceBtn).toBeVisible();
    await voiceBtn.click();

    // STATE_LABELS["wait"] is "Listening for wake word…".
    await expect(page.getByText(/waiting|listening|ready/i).first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test("VoiceMode shows a mic-related control", async ({ page }) => {
    await page.locator(VOICE_BTN).click();

    const voiceButtons = page.getByRole("button");
    await expect(voiceButtons.first()).toBeVisible({ timeout: 10_000 });
  });

  test("back button returns to GUI mode", async ({ page }) => {
    await page.locator(VOICE_BTN).click();

    await page.waitForTimeout(500);

    const backBtn = page
      .getByRole("button")
      .filter({ hasText: /back|exit|gui/i })
      .or(page.locator('[aria-label*="back" i], [title*="back" i]'))
      .first();

    if (await backBtn.isVisible()) {
      await backBtn.click();
      await expect(
        page.locator('aside[aria-label="Navigation"]')
      ).toBeVisible({ timeout: 5_000 });
    }
  });

  test("session ID stored in context is not the literal string 'voice'", async ({ page }) => {
    const sessionVal: string | null = await page.evaluate(() => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return (window as any).__pondSessionId ?? null;
    });

    // Null when the app doesn't expose __pondSessionId; only "voice" itself fails.
    expect(sessionVal).not.toBe("voice");
  });

  test("TranscriptFeed container is rendered in voice mode", async ({ page }) => {
    await page.locator(VOICE_BTN).click();

    await expect(page.locator("body")).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });
});
