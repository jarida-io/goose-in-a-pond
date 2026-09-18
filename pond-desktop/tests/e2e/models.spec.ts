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
import { navigateTo } from "./helpers/nav";

async function goToModels(page: Parameters<typeof mockAllApiRoutes>[0]) {
  await page.goto("/");
  // Models sits behind the drawer's "Manage" group; navigateTo expands it.
  await navigateTo(page, "Models");
  // No in-page view switch any more. The Set up / Manage toggle this helper
  // used to click was removed when the page was rebuilt (d516c82d); Models is
  // now a single scroll, so arriving at the section IS arriving at the content.
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

  test("memory status shows total MB when non-zero", async ({ page }) => {
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

    // Memory total should appear somewhere (8192 MB or 8 GB or similar)
    await expect(page.getByText(/8192|8,192|8\.0|8 GB/i).first()).toBeVisible({ timeout: 10_000 });
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

    // The spill warning badge appears for the too-large model.
    await expect(page.locator(".fit-badge").first()).toBeVisible({ timeout: 10_000 });
    await expect(page.locator(".fit-badge").first()).toContainText(/too large/i);

    // Exactly one badge — the fitting model must NOT be flagged.
    await expect(page.locator(".fit-badge")).toHaveCount(1);
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

    // The model list renders, but no spill badge appears (budget unknown).
    await expect(page.getByText(/gemma3n e2b/i).first()).toBeVisible({ timeout: 10_000 });
    await expect(page.locator(".fit-badge")).toHaveCount(0);
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

    // A progress indicator should be visible (progress bar or percentage text)
    await expect(
      page
        .locator('[role="progressbar"]')
        .or(page.getByText(/downloading|%|progress/i).first())
    ).toBeVisible({ timeout: 10_000 });
  });
});

// ── Live E2E tests (require running pond-server) ───────────────────────────────

const LIVE = !!process.env.GIAP_SERVER_URL;

test.describe("Models — live provider tests", () => {
  test.skip(!LIVE, "Set GIAP_SERVER_URL to run live provider tests");

  test("live memory status shows real data", async ({ page }) => {
    await page.goto(process.env.GIAP_SERVER_URL!);

    await navigateTo(page, "Models");

    // Memory status section should show non-zero data or "external"
    await expect(
      page.getByText(/MB|external|managed/i).first()
    ).toBeVisible({ timeout: 15_000 });
  });
});
