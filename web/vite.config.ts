import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const rootDir = fileURLToPath(new URL(".", import.meta.url));

const coopCoepDisabled =
  process.env.VITE_DISABLE_COOP_COEP === "1" || process.env.VITE_DISABLE_COOP_COEP === "true";

const crossOriginIsolationHeaders = {
  // Aero relies on SharedArrayBuffer + WASM threads, which require cross-origin isolation.
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
  "Cross-Origin-Resource-Policy": "same-origin",
} as const;

export default defineConfig({
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: resolve(rootDir, "index.html"),
        ipc_demo: resolve(rootDir, "demo/ipc_demo.html"),
      },
    },
  },
  server: {
    port: 5173,
    strictPort: true,
    headers: coopCoepDisabled ? undefined : crossOriginIsolationHeaders,
  },
  preview: {
    headers: coopCoepDisabled ? undefined : crossOriginIsolationHeaders,
  },
  test: {
    environment: "node",
    // Keep Vitest scoped to unit tests under src/. The repo also contains:
    //  - `web/test/*` which are Node's built-in `node:test` suites
    //  - `web/tests/*` which are Playwright e2e specs
    include: ["src/**/*.test.ts"],
    exclude: ["test/**", "tests/**"],
  },
});
