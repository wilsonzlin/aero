import { spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { mkdtemp } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { unrefBestEffort } from "../src/unrefSafe.ts";

const VITE_BUILD_TIMEOUT_MS = 180_000;

describe("repo-root Vite harness build outputs", () => {
  it(
    "emits AudioWorklet dependency assets used by the web runtime",
    async () => {
      const rootDir = fileURLToPath(new URL("../..", import.meta.url));
      const webDir = fileURLToPath(new URL("..", import.meta.url));
      const viteBin = path.join(webDir, "..", "node_modules", "vite", "bin", "vite.js");
      const outDir = await mkdtemp(path.join(os.tmpdir(), "aero-harness-dist-"));

      try {
        await new Promise<void>((resolve, reject) => {
          const child = spawn(
            process.execPath,
            [
              viteBin,
              "build",
              "--config",
              path.join(rootDir, "vite.harness.config.ts"),
              "--outDir",
              outDir,
            ],
            { cwd: rootDir, stdio: "inherit" },
          );

          const timer = setTimeout(() => {
            child.kill();
            reject(new Error("vite build (harness) timed out"));
          }, VITE_BUILD_TIMEOUT_MS);
          unrefBestEffort(timer);

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
            reject(new Error(`vite build (harness) failed (code=${code ?? "null"} signal=${signal ?? "null"})`));
          });
        });

        // Sanity-check the harness build ran.
        expect(existsSync(path.join(outDir, "aero.version.json"))).toBe(true);

        // `aero-d3d9` uses `#[wasm_bindgen(module = "/js/persistent_cache_shim.js")]`, so
        // prod builds must emit that absolute module path into `dist/`.
        const persistentCacheShimPath = path.join(outDir, "js", "persistent_cache_shim.js");
        expect(
          existsSync(persistentCacheShimPath),
          "Vite build must emit js/persistent_cache_shim.js for wasm-bindgen absolute module imports (/js/persistent_cache_shim.js). Did persistentCacheShimPlugin() get removed?",
        ).toBe(true);
        const persistentCacheShimSource = readFileSync(persistentCacheShimPath, "utf8");
        expect(
          persistentCacheShimSource,
          "js/persistent_cache_shim.js should contain wasm-bindgen exports like computeShaderCacheKey (sanity check for a non-empty/mis-emitted asset)",
        ).toContain("computeShaderCacheKey");
        expect(
          persistentCacheShimSource,
          "js/persistent_cache_shim.js should reference AeroPersistentGpuCache (ensure we emitted the actual shim implementation, not a broken re-export stub)",
        ).toContain("AeroPersistentGpuCache");

        // AudioWorklet dependency assets emitted explicitly because Vite doesn't follow ESM imports from worklets.
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
