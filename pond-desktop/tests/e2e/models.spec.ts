/**
 * Playwright E2E tests for the Models section.
 *
 * Covers:
 * - Provider tabs (LLM / ASR / TTS) are visible
 * - Active role pills show the assigned model
 * - Memory status is displayed
 * - Download progress bar shows during active downloads
 *
 * All tests use mocked API routes — no running pond-server required.
 *
 * Run: cd pond-desktop && npx playwright test tests/e2e/models.spec.ts
 */
import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

async function goToModels(page: Parameters<typeof mockAllApiRoutes>[0]) {
  await page.goto("/");
  const modelsBtn = page
    .getByRole("button", { name: /models/i })
    .or(page.locator('[title="Models"]'))
    .first();
  await modelsBtn.click({ timeout: 10_000 });
  // No tab hop any more. The screen used to open on a guided-setup wizard with
  // the real view behind a "Manage" tab; that split was removed deliberately —
  // see the header comment in `src/sections/Models.tsx` — so roles, memory and
  // downloads are on the section itself now.
}

test.describe("Models section", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("models section loads without errors", async ({ page }) => {
    await goToModels(page);
    // Should not show a crash / blank page
    await expect(page.locator("body")).not.toBeEmpty();
    // Some model-related text should be present
    await expect(
      page.getByText(/model|llm|asr|tts|llamafile|ollama|gguf/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("active chat role pill shows assigned model", async ({ page }) => {
    // Override active-roles to return a specific model assignment
    await page.route("**/api/v1/models/active-roles", (route) =>
      route.fulfill({
        json: {
          chat:  { provider: "llamafile", model: "llama3.2-3b" },
          think: { provider: "llamafile", model: "llama3.2-3b" },
          task:  { provider: "llamafile", model: "llama3.2-3b" },
          asr:   null,
          tts:   null,
        },
      }),
    );

    await goToModels(page);

    // The active model name should appear somewhere in the Models section
    await expect(
      page.getByText(/llama3\.2|llama3/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("memory status reports the budget a model can actually have", async ({ page }) => {
    await page.route("**/api/v1/models/memory-status", (route) =>
      route.fulfill({
        json: {
          total_mb: 8192,
          available_for_llm_mb: 4096,
          loaded_model: null,
        },
      }),
    );

    await goToModels(page);

    // The header reports what a model can ACTUALLY have — 4096 MB of budget,
    // rendered "4.0 GB" beside "for models" — not the 8192 MB device total.
    // That is deliberate: the fit meters measure against the usable figure,
    // and `fitReading` keeps the bar and the sentence agreeing about the same
    // model. So the raw total is not on this screen to assert.
    await expect(
      page.locator(".mdl-stat", { hasText: "for models" }),
    ).toContainText("4.0 GB", { timeout: 10_000 });
  });

  test("memory status shows loaded model name when a model is hot", async ({ page }) => {
    await page.route("**/api/v1/models/memory-status", (route) =>
      route.fulfill({
        json: {
          total_mb: 8192,
          available_for_llm_mb: 4000,
          loaded_model: "llama3.2-3b-instruct",
        },
      }),
    );

    await goToModels(page);

    await expect(
      page.getByText(/llama3\.2-3b|llama3/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("memory status shows zero / external label when total_mb is 0", async ({ page }) => {
    await page.route("**/api/v1/models/memory-status", (route) =>
      route.fulfill({
        json: { total_mb: 0, available_for_llm_mb: 0, loaded_model: null },
      }),
    );

    await goToModels(page);

    // Either "0" or an "external" / "managed externally" label
    await expect(
      page.getByText(/external|managed|0 MB|0MB|\b0\b/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("memory-fit guard: too-large model shows a spill warning", async ({ page }) => {
    // Budget: 4096 MB available → effective 3072 MB after 1 GB headroom.
    await page.route("**/api/v1/models/memory-status", (route) =>
      route.fulfill({
        json: { total_mb: 8192, available_for_llm_mb: 4096, loaded_model: null },
      }),
    );
    // One installed model that spills (5600 MB) and one that fits (1600 MB).
    await page.route("**/api/v1/models", (route) =>
      route.fulfill({
        json: {
          llamafile: [],
          gguf: [
            {
              category: "gguf",
              name: "gemma3n-e2b-toolarge",
              description: "gemma3n e2b (too large)",
              size_mb: 5600,
              downloaded: true,
              active: false,
            },
            {
              category: "gguf",
              name: "gemma-2-2b-fits",
              description: "gemma 2 2b (fits)",
              size_mb: 1600,
              downloaded: true,
              active: false,
            },
          ],
          whisper: [],
          tts: [],
          ollama: [],
          embedding: [],
        },
      }),
    );

    await goToModels(page);

    // The fit meter is now inline on every row rather than a badge that only
    // appears on a spill, and it carries its verdict in `title` (see
    // `fitReading` in src/sections/models/modelsView.ts). So the assertion is
    // no longer "how many badges" but "what does each row's meter say".
    const meters = page.locator(".mdl-row__fit");
    await expect(meters.first()).toBeVisible({ timeout: 10_000 });

    // The too-large model reports a spill; the fitting one must not.
    await expect(
      page.locator(".mdl-row__fit[title*='Bigger than']"),
    ).toHaveCount(1);
    await expect(page.locator(".mdl-row__fit[title*='Uses']")).toHaveCount(1);
  });

  test("memory-fit guard: no warning when budget is unavailable (Mac/dev)", async ({ page }) => {
    // NoopScheduler / Mac dev reports zeros → no verdict, no badge.
    await page.route("**/api/v1/models/memory-status", (route) =>
      route.fulfill({
        json: { total_mb: 0, available_for_llm_mb: 0, loaded_model: null },
      }),
    );
    await page.route("**/api/v1/models", (route) =>
      route.fulfill({
        json: {
          llamafile: [],
          gguf: [
            {
              category: "gguf",
              name: "gemma3n-e2b-toolarge",
              description: "gemma3n e2b (too large)",
              size_mb: 5600,
              downloaded: true,
              active: false,
            },
          ],
          whisper: [],
          tts: [],
          ollama: [],
          embedding: [],
        },
      }),
    );

    await goToModels(page);

    // The model list renders, and the meter declines to give a verdict rather
    // than drawing a confident bar from nothing — "a confident bar drawn from
    // nothing is worse than no bar" (src/sections/Models.tsx). The element is
    // present either way now, so the claim is about what it says.
    await expect(page.getByText(/gemma3n e2b/i).first()).toBeVisible({ timeout: 10_000 });
    await expect(
      page.locator(".mdl-row__fit[title*='Size unknown']"),
    ).toHaveCount(1);
    await expect(page.locator(".mdl-row__fit[title*='Bigger than']")).toHaveCount(0);
  });

  test("download progress bar visible when download in progress", async ({ page }) => {
    await page.route("**/api/v1/models/download/progress", (route) =>
      route.fulfill({
        json: {
          downloads: [
            {
              filename: "llama3.2-3b.gguf",
              category: "gguf",
              downloaded_bytes: 512_000_000,
              total_bytes: 2_000_000_000,
              status: "downloading",
            },
          ],
        },
      }),
    );

    await page.route("**/api/v1/models", (route) =>
      route.fulfill({
        json: {
          llamafile: [],
          gguf: [
            {
              category: "gguf",
              name: "llama3.2-3b",
              description: "3B parameter model",
              size_mb: 2000,
              downloaded: false,
              active: false,
              url: "https://example.com/llama3.2-3b.gguf",
              filename: "llama3.2-3b.gguf",
            },
          ],
          whisper: [],
          tts: [],
        },
      }),
    );

    await goToModels(page);

    // The bar carries a role and an accessible name now, so it can be named
    // directly. The old `.or(getByText(/%|progress/))` fallback dated from
    // markup that had neither, and matched both the bar and its own "26%"
    // label — two elements, which is a strict-mode violation rather than a
    // missing indicator.
    await expect(
      page.getByRole("progressbar", { name: /llama3\.2-3b\.gguf download progress/i }),
    ).toBeVisible({ timeout: 10_000 });
  });
});

// ── Live E2E tests (require running pond-server) ───────────────────────────────

const LIVE = !!process.env.GIAP_SERVER_URL;

test.describe("Models — live provider tests", () => {
  test.skip(!LIVE, "Set GIAP_SERVER_URL to run live provider tests");

  test("live memory status shows real data", async ({ page }) => {
    await page.goto(process.env.GIAP_SERVER_URL!);

    const modelsBtn = page.getByRole("button", { name: /models/i }).first();
    await modelsBtn.click({ timeout: 10_000 });

    // Memory status section should show non-zero data or "external"
    await expect(
      page.getByText(/MB|external|managed/i).first()
    ).toBeVisible({ timeout: 15_000 });
  });
});
