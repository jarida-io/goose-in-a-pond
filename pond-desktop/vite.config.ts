import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "path";

export default defineConfig(async () => ({
  plugins: [tailwindcss(), react()],

  // Pin React to pond-desktop's own node_modules so web/ and pond-desktop/ never mix instances.
  resolve: {
    alias: [
      { find: "react/jsx-runtime",     replacement: resolve(__dirname, "node_modules/react/jsx-runtime.js") },
      { find: "react/jsx-dev-runtime", replacement: resolve(__dirname, "node_modules/react/jsx-dev-runtime.js") },
      { find: "react-dom/client",      replacement: resolve(__dirname, "node_modules/react-dom/client.js") },
      { find: "react-dom/server",      replacement: resolve(__dirname, "node_modules/react-dom/server.js") },
      { find: "react-dom",             replacement: resolve(__dirname, "node_modules/react-dom/index.js") },
      { find: "react",                 replacement: resolve(__dirname, "node_modules/react/index.js") },
    ],
    dedupe: ["react", "react-dom"],
  },

  // Fixed port: `dev:electron` loads the renderer from localhost:1420.
  server: {
    port: 1420,
    strictPort: true,
    host: "localhost",
    hmr: {
      protocol: "ws",
      host: "localhost",
      port: 1421,
    },
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },

  // Prevent Vite from hiding Rust compilation errors
  clearScreen: false,
}));
