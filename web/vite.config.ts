import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";

import {
  baselineSecurityHeaders,
  crossOriginIsolationHeaders,
  cspHeaders,
} from "../scripts/security_headers.mjs";

const rootDir = fileURLToPath(new URL(".", import.meta.url));

const coopCoepSetting = (process.env.VITE_DISABLE_COOP_COEP ?? "").toLowerCase();
const coopCoepDisabled = coopCoepSetting === "1" || coopCoepSetting === "true";

type AeroBuildInfo = Readonly<{
  version: string;
  gitSha: string;
  builtAt: string;
}>;

function resolveGitSha(): string {
  const fromEnv = process.env.GIT_SHA || process.env.GITHUB_SHA;
  if (fromEnv && fromEnv.trim().length > 0) return fromEnv.trim();

  try {
    return execSync("git rev-parse HEAD", { cwd: rootDir, encoding: "utf8" }).trim();
  } catch {
    return "dev";
  }
}

function resolveBuildTimestamp(): string {
  const explicit = process.env.BUILD_TIMESTAMP;
  if (explicit && explicit.trim().length > 0) return explicit.trim();

  // Support reproducible builds when SOURCE_DATE_EPOCH is set (common in release pipelines).
  const sourceDateEpoch = process.env.SOURCE_DATE_EPOCH;
  if (sourceDateEpoch && /^\d+$/.test(sourceDateEpoch)) {
    return new Date(Number(sourceDateEpoch) * 1000).toISOString();
  }

  return new Date().toISOString();
}

function resolveVersion(gitSha: string): string {
  const fromEnv = process.env.AERO_VERSION || process.env.GITHUB_REF_NAME;
  if (fromEnv && fromEnv.trim().length > 0) return fromEnv.trim();
  return gitSha.length ? gitSha.slice(0, 12) : "dev";
}

function aeroBuildInfoPlugin(): Plugin {
  const gitSha = resolveGitSha();
  const buildInfo: AeroBuildInfo = {
    version: resolveVersion(gitSha),
    gitSha,
    builtAt: resolveBuildTimestamp(),
  };

  const jsonBody = `${JSON.stringify(buildInfo, null, 2)}\n`;

  return {
    name: "aero-build-info",
    config: () => ({
      define: {
        __AERO_BUILD_INFO__: JSON.stringify(buildInfo),
      },
    }),
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "aero.version.json",
        source: jsonBody,
      });
    },
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const pathname = req.url?.split("?", 1)[0];
        if (pathname !== "/aero.version.json") return next();
        res.statusCode = 200;
        res.setHeader("Content-Type", "application/json; charset=utf-8");
        res.end(jsonBody);
      });
    },
  };
}
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

function audioWorkletDependenciesPlugin(): Plugin {
  // Vite treats AudioWorklet modules loaded via `audioWorklet.addModule(new URL(...))` as static
  // assets and does not follow their ESM imports. Our mic worklet (`src/audio/mic-worklet-processor.js`)
  // imports `./mic_ring.js`, so we manually emit a copy into `dist/assets/` so the browser can
  // resolve it at runtime.
  const srcMicRingPath = resolve(rootDir, "src/audio/mic_ring.js");
  const source = readFileSync(srcMicRingPath, "utf8");
  return {
    name: "aero-audio-worklet-deps",
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "assets/mic_ring.js",
        source,
      });
    },
  };
}

export default defineConfig({
  assetsInclude: ["**/*.wasm"],
  plugins: [aeroBuildInfoPlugin(), wasmMimeTypePlugin(), audioWorkletDependenciesPlugin()],
  server: {
    port: 5173,
    strictPort: true,
    // Do not set a strict CSP on the dev server; it can interfere with HMR.
    headers: {
      ...(coopCoepDisabled ? {} : crossOriginIsolationHeaders),
      ...baselineSecurityHeaders,
    },
  },
  preview: {
    headers: {
      ...(coopCoepDisabled ? {} : crossOriginIsolationHeaders),
      ...baselineSecurityHeaders,
      ...cspHeaders,
    },
  },
  worker: {
    format: "es",
  },
  test: {
    environment: "node",
    // Keep Vitest scoped to unit tests under src/, plus any dedicated Vitest
    // suites under `web/test/` (suffixed `.vitest.ts`). The repo also contains:
    //  - `web/test/*.test.ts` which are Node's built-in `node:test` suites
    //  - `web/tests/*` which are Playwright e2e specs
    include: ["src/**/*.test.ts", "test/**/*.vitest.ts"],
    exclude: ["test/**/*.test.ts", "tests/**"],
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
        debug: resolve(rootDir, "debug.html"),
        ipc_demo: resolve(rootDir, "demo/ipc_demo.html"),
        webusb_diagnostics: resolve(rootDir, "webusb_diagnostics.html"),
      },
    },
  },
});
