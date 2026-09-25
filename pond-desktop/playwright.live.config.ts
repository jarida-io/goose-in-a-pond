import { defineConfig, devices } from "@playwright/test";

/**
 * Live tests against a real pond-server, no mocks. Start with `scripts/live-test.sh --ui`,
 * which owns the server (hence no `webServer` block) and sets POND_LIVE_URL.
 */

const baseURL = process.env.POND_LIVE_URL ?? "http://127.0.0.1:4000";

export default defineConfig({
  testDir: "./tests/e2e-live",
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  expect: { timeout: 15_000 },
  // No retries: a second-attempt pass signals a startup-ordering bug.
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL,
    headless: true,
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
