import { expect, test } from "@playwright/test";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

test("aero-gpu-wasm: destroy_gpu resets submit_aerogpu command processor state", async ({ page }) => {
  await page.goto("/web/blank.html");

  const thisDir = dirname(fileURLToPath(import.meta.url));
  const repoRoot = dirname(dirname(dirname(thisDir)));
  const bundles = [
    {
      js: join(repoRoot, "web", "src", "wasm", "pkg-single-gpu", "aero_gpu_wasm.js"),
      wasm: join(repoRoot, "web", "src", "wasm", "pkg-single-gpu", "aero_gpu_wasm_bg.wasm"),
    },
    {
      js: join(repoRoot, "web", "src", "wasm", "pkg-threaded-gpu", "aero_gpu_wasm.js"),
      wasm: join(repoRoot, "web", "src", "wasm", "pkg-threaded-gpu", "aero_gpu_wasm_bg.wasm"),
    },
    {
      js: join(repoRoot, "web", "src", "wasm", "pkg-single-gpu-dev", "aero_gpu_wasm.js"),
      wasm: join(repoRoot, "web", "src", "wasm", "pkg-single-gpu-dev", "aero_gpu_wasm_bg.wasm"),
    },
    {
      js: join(repoRoot, "web", "src", "wasm", "pkg-threaded-gpu-dev", "aero_gpu_wasm.js"),
      wasm: join(repoRoot, "web", "src", "wasm", "pkg-threaded-gpu-dev", "aero_gpu_wasm_bg.wasm"),
    },
  ];
  if (!bundles.some(({ js, wasm }) => existsSync(js) && existsSync(wasm))) {
    const message = [
      "aero-gpu-wasm bundle is missing.",
      "",
      "Expected one of:",
      ...bundles.flatMap(({ js, wasm }) => [`- ${wasm}`, `  ${js}`]),
      "",
      "Build it with (from the repo root):",
      "  npm -w web run wasm:build",
    ].join("\n");
    if (process.env.CI) {
      throw new Error(message);
    }
    test.skip(true, message);
  }

  await page.setContent(`
    <script type="module">
      import initAeroGpuWasm, { destroy_gpu, submit_aerogpu } from "/web/src/wasm/aero-gpu.ts";
      import {
        AerogpuCmdWriter,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
      } from "/emulator/protocol/aerogpu/aerogpu_cmd.ts";
      import { AerogpuFormat } from "/emulator/protocol/aerogpu/aerogpu_pci.ts";
      import { formatOneLineUtf8 } from "/web/src/text.ts";

      const MAX_ERROR_BYTES = 512;

      function formatOneLineError(err) {
        const msg = err instanceof Error ? err.message : err;
        return formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error";
      }

      (async () => {
        try {
          await initAeroGpuWasm();

          const makeStream = (w) => {
            const writer = new AerogpuCmdWriter();
            writer.createTexture2d(
              /* textureHandle */ 1,
              /* usageFlags */ AEROGPU_RESOURCE_USAGE_TEXTURE,
              AerogpuFormat.R8G8B8A8Unorm,
              w >>> 0,
              64,
              1,
              1,
              (w * 4) >>> 0,
              0,
              0,
            );
            writer.present(0, 0);
            return writer.finish();
          };

          const stream0 = makeStream(64);
          const stream1 = makeStream(32);

          /** @type {any} */
          let result0 = null;
          /** @type {any} */
          let result1 = null;
          /** @type {string | null} */
          let err1 = null;

          result0 = submit_aerogpu(stream0, 1n);
          destroy_gpu();
          try {
            result1 = submit_aerogpu(stream1, 2n);
          } catch (err) {
            err1 = formatOneLineError(err);
          }

          window.__AERO_WASM_DESTROY_GPU_RESULT__ = {
            result0,
            result1,
            err1,
          };
        } catch (err) {
          window.__AERO_WASM_DESTROY_GPU_RESULT__ = { error: formatOneLineError(err) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_WASM_DESTROY_GPU_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_WASM_DESTROY_GPU_RESULT__);

  expect(result.error ?? null).toBeNull();
  expect(result.err1 ?? null).toBeNull();
  expect(result.result0?.presentCount ?? null).toBe(1n);
  // destroy_gpu() should reset the monotonic present counter for submit_aerogpu().
  expect(result.result1?.presentCount ?? null).toBe(1n);
});
