import { defineConfig } from "vitest/config";

// Two projects: renderer on happy-dom; main process on plain Node, as it runs in Electron.
export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: "renderer",
          environment: "happy-dom",
          include: ["src/**/*.test.{ts,tsx}"],
          setupFiles: ["src/test-setup.ts"],
        },
      },
      {
        test: {
          name: "main",
          environment: "node",
          include: ["electron/**/*.test.ts"],
        },
      },
    ],
  },
});
