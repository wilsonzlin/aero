import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";

const rootDir = fileURLToPath(new URL(".", import.meta.url));

const coopCoepDisabled =
  process.env.VITE_DISABLE_COOP_COEP === "1" || process.env.VITE_DISABLE_COOP_COEP === "true";

const crossOriginIsolationHeaders = {
  // Aero relies on SharedArrayBuffer + WASM threads, which require cross-origin isolation.
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
  // Avoid COEP failures for same-origin assets (useful in dev/preview with workers).
  "Cross-Origin-Resource-Policy": "same-origin",
  "Origin-Agent-Cluster": "?1",
} as const;

const commonSecurityHeaders = {
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "no-referrer",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
} as const;

const previewOnlyHeaders = {
  // Match the default CSP used by the static hosting templates. Keep `connect-src`
  // narrow; deployments that use a separate proxy origin should add it explicitly.
  "Content-Security-Policy":
    "default-src 'none'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; connect-src 'self' https://aero-gateway.invalid wss://aero-gateway.invalid; img-src 'self' data: blob:; style-src 'self'; font-src 'self'",
} as const;

function wasmMimeTypePlugin(): Plugin {
  const setWasmHeader: Plugin["configureServer"] = (server) => {
    server.middlewares.use((req, res, next) => {
      // `instantiateStreaming` requires the correct MIME type.
      const pathname = req.url?.split("?", 1)[0];
      if (pathname?.endsWith(".wasm")) {
        res.setHeader("Content-Type", "application/wasm");
      }
      next();
    });
  };

  return {
    name: "wasm-mime-type",
    configureServer: setWasmHeader,
    configurePreviewServer: setWasmHeader,
  };
}

export default defineConfig({
  assetsInclude: ["**/*.wasm"],
  plugins: [wasmMimeTypePlugin()],
  server: {
    port: 5173,
    strictPort: true,
    // Do not set a strict CSP on the dev server; it can interfere with HMR.
    headers: {
      ...(coopCoepDisabled ? {} : crossOriginIsolationHeaders),
      ...commonSecurityHeaders,
    },
  },
  preview: {
    headers: {
      ...(coopCoepDisabled ? {} : crossOriginIsolationHeaders),
      ...commonSecurityHeaders,
      ...previewOnlyHeaders,
    },
  },
  worker: {
    format: "es",
  },
  test: {
    environment: "node",
    // Keep Vitest scoped to unit tests under src/. The repo also contains:
    //  - `web/test/*` which are Node's built-in `node:test` suites
    //  - `web/tests/*` which are Playwright e2e specs
    include: ["src/**/*.test.ts"],
    exclude: ["test/**", "tests/**"],
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // The real emulator WASM will be large, but our demo module is tiny.
    // Force `.wasm` to be emitted as a file so `fetch()`/`instantiateStreaming()`
    // behaves the same in `vite preview` as it does in `vite dev`.
    assetsInlineLimit: 0,
    rollupOptions: {
      input: {
        main: resolve(rootDir, "index.html"),
        ipc_demo: resolve(rootDir, "demo/ipc_demo.html"),
      },
    },
  },
});
