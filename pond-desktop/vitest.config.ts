import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "happy-dom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
    // Several settings tests read the device's own zone through
    // `Intl.DateTimeFormat().resolvedOptions().timeZone` — the location name a
    // detected pond gets, and the "use the device's zone" offer that only
    // appears when it differs from the stored one. Left to the host, those
    // tests pass on a developer machine in this zone and fail on a UTC CI
    // runner, where the offer is correctly absent because nothing differs.
    // Pinning the zone makes them assert behaviour rather than geography.
    env: { TZ: "Africa/Nairobi" },
  },
});
