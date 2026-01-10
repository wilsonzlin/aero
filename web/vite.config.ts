import { defineConfig } from "vite";

export default defineConfig({
  server: {
    // Aero relies on SharedArrayBuffer + WASM threads, which require cross-origin isolation.
    headers: {
      "Cross-Origin-Opener-Policy": "same-origin",
      "Cross-Origin-Embedder-Policy": "require-corp",
    },
  },
});

