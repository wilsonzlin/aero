import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import type http from "node:http";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";

import {
  baselineSecurityHeaders,
  crossOriginIsolationHeaders,
  cspHeaders,
} from "../scripts/security_headers.mjs";
import { destroyBestEffort } from "../src/socket_safe.js";

const rootDir = fileURLToPath(new URL(".", import.meta.url));

const coopCoepSetting = (process.env.VITE_DISABLE_COOP_COEP ?? "").toLowerCase();
const coopCoepDisabled = coopCoepSetting === "1" || coopCoepSetting === "true";

const nodeMajor = (() => {
  const major = Number.parseInt(process.versions.node.split(".", 1)[0] ?? "", 10);
  return Number.isFinite(major) ? major : 0;
})();

const vitestMaxForks = (() => {
  // `vitest` uses Tinypool + `child_process.fork()` when `pool: "forks"` is enabled below.
  //
  // In sandboxed environments with tight process/thread limits, newer Node majors can fail to
  // spawn additional forks with:
  //   - `uv_thread_create` assertion failures, or
  //   - `spawn /usr/bin/node EAGAIN`
  //
  // Keep Node 22.x (CI baseline) reasonably parallel, but cap newer majors more aggressively so
  // unit tests remain runnable for contributors / hermetic runners.
  if (nodeMajor >= 25) return 2;
  if (nodeMajor >= 23) return 4;
  return 8;
})();

const vitestTestTimeoutMs = (() => {
  // Some of our unit tests dynamically import worker bundles (and their WASM dependencies) during
  // the test body. On newer Node majors (or under cold caches / constrained sandboxes), that
  // overhead can exceed Vitest's default 5s per-test timeout.
  //
  // Keep CI baseline (Node 22.x) tight, but bump newer majors so `npm -w web run test:unit` is
  // runnable for contributors / hermetic runners.
  if (nodeMajor >= 25) return 20_000;
  return 5_000;
})();

type AeroBuildInfo = Readonly<{
  version: string;
  gitSha: string;
  builtAt: string;
}>;

function resolveGitSha(): string {
  const fromEnv = process.env.GIT_SHA || process.env.GITHUB_SHA;
  if (fromEnv && fromEnv.trim().length > 0) return fromEnv.trim();

  try {
    return execFileSync("git", ["rev-parse", "HEAD"], { cwd: rootDir, encoding: "utf8" }).trim();
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
      server.middlewares.use((req: http.IncomingMessage, res: http.ServerResponse, next: () => void) => {
        const pathname = req.url?.split("?", 1)[0];
        if (pathname !== "/aero.version.json") return next();
        try {
          res.statusCode = 200;
          res.setHeader("Content-Type", "application/json; charset=utf-8");
          res.end(jsonBody);
        } catch {
          destroyBestEffort(res);
        }
      });
    },
  };
}
function wasmMimeTypePlugin(): Plugin {
  const installWasmMiddleware = (middlewares: { use: (...args: any[]) => any }) => {
    middlewares.use((req: http.IncomingMessage, res: http.ServerResponse, next: () => void) => {
      // `instantiateStreaming` requires the correct MIME type.
      const pathname = req.url?.split("?", 1)[0];
      if (pathname?.endsWith(".wasm")) {
        try {
          res.setHeader("Content-Type", "application/wasm");
        } catch {
          destroyBestEffort(res);
          return;
        }
      }
      next();
    });
  };

  return {
    name: "wasm-mime-type",
    configureServer(server) {
      installWasmMiddleware(server.middlewares);
    },
    configurePreviewServer(server) {
      installWasmMiddleware(server.middlewares);
    },
  };
}

function audioWorkletDependenciesPlugin(): Plugin {
  // Vite treats AudioWorklet modules loaded via `audioWorklet.addModule(new URL(...))` as static
  // assets and does not follow their ESM imports. Our worklets have runtime ESM imports:
  // - `src/audio/mic-worklet-processor.js` imports `./mic_ring.js`
  // - `src/platform/audio-worklet-processor.js` imports `./audio_worklet_ring_layout.js`
  //
  // Emit copies into `dist/assets/` so the browser can resolve them at runtime.
  const srcMicRingPath = resolve(rootDir, "src/audio/mic_ring.js");
  const source = readFileSync(srcMicRingPath, "utf8");
  const srcAudioWorkletRingLayoutPath = resolve(rootDir, "src/platform/audio_worklet_ring_layout.js");
  const audioWorkletRingLayoutSource = readFileSync(srcAudioWorkletRingLayoutPath, "utf8");
  return {
    name: "aero-audio-worklet-deps",
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "assets/mic_ring.js",
        source,
      });
      this.emitFile({
        type: "asset",
        fileName: "assets/audio_worklet_ring_layout.js",
        source: audioWorkletRingLayoutSource,
      });
    },
  };
}

function persistentCacheShimPlugin(): Plugin {
  // `aero-d3d9` uses `#[wasm_bindgen(module = "/js/persistent_cache_shim.js")]`,
  // so we need to ensure that file exists in dev/preview and in the build output.
  const srcShimPath = resolve(rootDir, "../crates/aero-d3d9/js/persistent_cache_shim.js");
  const source = readFileSync(srcShimPath, "utf8");

  const installShimMiddleware = (middlewares: { use: (...args: any[]) => any }) => {
    middlewares.use(
      (
        req: http.IncomingMessage,
        res: http.ServerResponse,
        next: () => void,
      ) => {
        const pathname = req.url?.split("?", 1)[0];
        if (pathname !== "/js/persistent_cache_shim.js") return next();
        try {
          res.statusCode = 200;
          res.setHeader("Content-Type", "text/javascript; charset=utf-8");
          res.end(source);
        } catch {
          destroyBestEffort(res);
        }
      },
    );
  };

  return {
    name: "aero-persistent-cache-shim",
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "js/persistent_cache_shim.js",
        source,
      });
    },
    configureServer(server) {
      installShimMiddleware(server.middlewares);
    },
    configurePreviewServer(server) {
      installShimMiddleware(server.middlewares);
    },
  };
}

export default defineConfig({
  assetsInclude: ["**/*.wasm"],
  plugins: [
    aeroBuildInfoPlugin(),
    wasmMimeTypePlugin(),
    audioWorkletDependenciesPlugin(),
    persistentCacheShimPlugin(),
  ],
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
    testTimeout: vitestTestTimeoutMs,
    // These tests exercise blocking Atomics.wait() + Node worker_threads.
    // On newer Node releases, Vitest's default thread pool can interfere with
    // nested Worker scheduling / Atomics wakeups, causing flakes/timeouts.
    // Run tests in forked processes for deterministic cross-thread behavior.
    pool: "forks",
    // Vitest defaults its pool size based on `os.cpus()`, which can be extremely large in
    // sandbox environments (e.g. 192 vCPUs). Spawning that many Node processes can exhaust
    // memory / pthread resources and crash workers mid-run.
    //
    // Cap the fork count so `npm test` remains stable even when the host reports very high
    // core counts.
    poolOptions: {
      forks: {
        minForks: 1,
        // Keep this conservative: each forked Node process spawns its own worker threads.
        // In heavily sandboxed environments, even a handful of extra threads can push the
        // process over the pthread/rlimit ceiling and crash the Vitest runner.
        //
        // CI uses Node 22.x; newer majors can be more thread-hungry / unstable in sandboxed
        // environments, so cap them more aggressively.
        maxForks: vitestMaxForks,
      },
    },
    // Keep Vitest scoped to unit tests under src/, plus any dedicated Vitest
    // suites under `web/test/` (suffixed `.vitest.ts`). The repo also contains:
    //  - `web/test/*.test.ts` which are Node's built-in `node:test` suites
    //  - `tests/e2e/*` which are Playwright specs
    include: ["src/**/*.test.ts", "test/**/*.vitest.ts"],
    exclude: ["test/**/*.test.ts", "tests/**"],
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov"],
      // Write coverage reports to the repo root (shared with Rust coverage, Codecov, etc).
      reportsDirectory: resolve(rootDir, "../coverage"),
      include: ["src/**/*.ts"],
      exclude: ["src/**/*.d.ts"],
    },
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
        webgl2_fallback_demo: resolve(rootDir, "webgl2_fallback_demo.html"),
        wddm_scanout_smoke: resolve(rootDir, "wddm-scanout-smoke.html"),
        wddm_scanout_vram_smoke: resolve(rootDir, "wddm-scanout-vram-smoke.html"),
        wddm_scanout_debug: resolve(rootDir, "wddm-scanout-debug.html"),
      },
    },
  },
});
