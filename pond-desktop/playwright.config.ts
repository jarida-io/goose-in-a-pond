import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright E2E configuration for pond-desktop.
 *
 * Tests run against the Vite dev server (npm run dev → http://localhost:5173).
 * In non-Tauri context, AppContext.tsx marks server online immediately so
 * all sections render without requiring a running pond-server process.
 *
 * API calls are intercepted via page.route() mocks in each test.
 *
 * Run: npx playwright test
 * Run with UI: npx playwright test --ui
 */

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  retries: 0,
  reporter: [["list"], ["html", { outputFolder: "playwright-report", open: "never" }]],
  use: {
    baseURL: "http://localhost:5173",
    headless: true,
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    trace: "on-first-retry",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        launchOptions: {
          args: [
            "--autoplay-policy=no-user-gesture-required",
            "--use-fake-ui-for-media-stream",
            "--use-fake-device-for-media-stream",
          ],
        },
      },
    },
  ],
  // Automatically start the Vite dev server before running tests.
  // Use dev:vite (Vite only) rather than dev (Vite + pond-server) so that
  // the --port flag is not forwarded to pond-server, which rejects it.
  webServer: {
    command: "npm run dev:vite -- --port 5173",
    url: "http://localhost:5173",
    reuseExistingServer: !process.env["CI"],
    timeout: 60_000,
    stdout: "pipe",
    stderr: "pipe",
  },
});
