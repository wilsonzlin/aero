import { spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { mkdtemp } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const VITE_BUILD_TIMEOUT_MS = 120_000;

describe("web Vite build outputs", () => {
  it(
    "emits standalone HTML pages into dist",
    async () => {
    const webDir = fileURLToPath(new URL("..", import.meta.url));
    const viteBin = path.join(webDir, "..", "node_modules", "vite", "bin", "vite.js");
    const outDir = await mkdtemp(path.join(os.tmpdir(), "aero-web-dist-"));

    try {
      // Run via spawn so the Vitest worker event loop stays responsive during the build.
      // Blocking calls like `execFileSync` can stall worker RPC traffic and surface as
      // unhandled "Timeout calling onTaskUpdate" errors.
      await new Promise<void>((resolve, reject) => {
        const child = spawn(
          process.execPath,
          [viteBin, "build", "--config", path.join(webDir, "vite.config.ts"), "--outDir", outDir],
          { cwd: webDir, stdio: "inherit" },
        );

        // Guard against hangs (e.g. if Vite config/plugins accidentally start a watch).
        const timer = setTimeout(() => {
          child.kill();
          reject(new Error("vite build timed out"));
        }, VITE_BUILD_TIMEOUT_MS);
        (timer as unknown as { unref?: () => void }).unref?.();

        child.on("error", (err) => {
          clearTimeout(timer);
          reject(err);
        });

        child.on("exit", (code, signal) => {
          clearTimeout(timer);
          if (code === 0) {
            resolve();
            return;
          }
          reject(new Error(`vite build failed (code=${code ?? "null"} signal=${signal ?? "null"})`));
        });
      });

      expect(existsSync(path.join(outDir, "webusb_diagnostics.html"))).toBe(true);
      expect(existsSync(path.join(outDir, "webgl2_fallback_demo.html"))).toBe(true);
      // `aero-d3d9` uses `#[wasm_bindgen(module = "/js/persistent_cache_shim.js")]`, so
      // prod builds must emit that absolute module path into `dist/`.
      const persistentCacheShimPath = path.join(outDir, "js", "persistent_cache_shim.js");
      expect(existsSync(persistentCacheShimPath)).toBe(true);
      const persistentCacheShimSource = readFileSync(persistentCacheShimPath, "utf8");
      expect(persistentCacheShimSource).toContain("computeShaderCacheKey");
      // AudioWorklet modules are emitted as static assets; ensure their unbundled
      // dependency files are also present.
      expect(existsSync(path.join(outDir, "assets", "mic_ring.js"))).toBe(true);
      expect(existsSync(path.join(outDir, "assets", "audio_worklet_ring_layout.js"))).toBe(true);

      const assetsDir = path.join(outDir, "assets");
      const assets = new Set(readdirSync(assetsDir));

      const audioWorklet = [...assets].find((name) => /^audio-worklet-processor(?:-.*)?\.js$/.test(name));
      expect(audioWorklet).toBeTruthy();
      const audioWorkletSource = readFileSync(path.join(assetsDir, audioWorklet!), "utf8");
      expect(audioWorkletSource).toContain("./audio_worklet_ring_layout.js");

      const micWorklet = [...assets].find((name) => /^mic-worklet-processor(?:-.*)?\.js$/.test(name));
      expect(micWorklet).toBeTruthy();
      const micWorkletSource = readFileSync(path.join(assetsDir, micWorklet!), "utf8");
      expect(micWorkletSource).toContain("./mic_ring.js");
    } finally {
      rmSync(outDir, { recursive: true, force: true });
    }
    },
    VITE_BUILD_TIMEOUT_MS,
  );
});
