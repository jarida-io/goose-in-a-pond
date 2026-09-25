import { defineConfig } from "tsup";

// Main process and preload only; the renderer stays on Vite (pond-server embeds its output).
// CJS because a sandboxed preload must be CommonJS.
export default defineConfig({
  entry: ["electron/main/index.ts", "electron/preload/index.ts"],
  outDir: "dist-electron",
  format: "cjs",
  platform: "node",
  target: "node20",
  external: ["electron"],
  clean: true,
  sourcemap: true,
  // package.json is "type": "module", so a .js bundle would load as ESM; the extension decides.
  outExtension: () => ({ js: ".cjs" }),
});
