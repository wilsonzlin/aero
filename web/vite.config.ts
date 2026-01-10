import { resolve } from "node:path";
import { defineConfig } from "vite";

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
  preview: {
    headers: {
      "Cross-Origin-Opener-Policy": "same-origin",
      "Cross-Origin-Embedder-Policy": "require-corp",
    },
  },
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        ipc_demo: resolve(__dirname, "demo/ipc_demo.html"),
      },
    },
  },
});
