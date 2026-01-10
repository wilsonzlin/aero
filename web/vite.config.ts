import { defineConfig } from "vite";

const crossOriginIsolationHeaders = {
  // Aero relies on SharedArrayBuffer + WASM threads, which require cross-origin isolation.
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
} as const;

export default defineConfig({
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    headers: crossOriginIsolationHeaders,
  },
  preview: {
    headers: crossOriginIsolationHeaders,
  },
  test: {
    environment: "node",
  },
});
