import { expect, test } from "@playwright/test";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

test("aero-gpu-wasm: submit_aerogpu decodes alloc_table bytes for backing_alloc_id validation", async ({ page }) => {
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
      import initAeroGpuWasm, { submit_aerogpu } from "/web/src/wasm/aero-gpu.ts";
      import { AerogpuCmdWriter } from "/emulator/protocol/aerogpu/aerogpu_cmd.ts";
      import { AEROGPU_ALLOC_TABLE_MAGIC } from "/emulator/protocol/aerogpu/aerogpu_ring.ts";
      import { AEROGPU_ABI_VERSION_U32 } from "/emulator/protocol/aerogpu/aerogpu_pci.ts";
      import { formatOneLineUtf8 } from "/web/src/text.ts";

      const MAX_ERROR_BYTES = 512;

      function formatOneLineError(err) {
        const msg = err instanceof Error ? err.message : err;
        return formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error";
      }

      function buildAllocTable(allocId, gpa, sizeBytes) {
        const headerBytes = 24;
        const entryStrideBytes = 32;
        const size = headerBytes + entryStrideBytes;
        const buf = new ArrayBuffer(size);
        const dv = new DataView(buf);
        dv.setUint32(0, AEROGPU_ALLOC_TABLE_MAGIC, true);
        dv.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
        dv.setUint32(8, size, true);
        dv.setUint32(12, 1, true);
        dv.setUint32(16, entryStrideBytes, true);
        dv.setUint32(20, 0, true);

        dv.setUint32(24 + 0, allocId >>> 0, true);
        dv.setUint32(24 + 4, 0, true);
        dv.setBigUint64(24 + 8, gpa, true);
        dv.setBigUint64(24 + 16, sizeBytes, true);
        dv.setBigUint64(24 + 24, 0n, true);
        return new Uint8Array(buf);
      }

      (async () => {
        try {
          await initAeroGpuWasm();

          // A command stream that references backing_alloc_id=1.
          const writer = new AerogpuCmdWriter();
          writer.createBuffer(
            /* bufferHandle */ 1,
            /* usageFlags */ 0,
            /* sizeBytes */ 16n,
            /* backingAllocId */ 1,
            /* backingOffsetBytes */ 0,
          );
          writer.present(0, 0);
          const cmdStream = writer.finish();

          let missingError = null;
          try {
            submit_aerogpu(cmdStream, 1n);
          } catch (err) {
            missingError = formatOneLineError(err);
          }

          const allocTable = buildAllocTable(/* allocId */ 1, /* gpa */ 0n, /* sizeBytes */ 4096n);
          const okResult = submit_aerogpu(cmdStream, 2n, allocTable);

          window.__AERO_WASM_ALLOC_TABLE_RESULT__ = {
            missingError,
            okResult,
          };
        } catch (err) {
          window.__AERO_WASM_ALLOC_TABLE_RESULT__ = { error: formatOneLineError(err) };
        }
      })();
    </script>
  `);

  await page.waitForFunction(() => (window as any).__AERO_WASM_ALLOC_TABLE_RESULT__);
  const result = await page.evaluate(() => (window as any).__AERO_WASM_ALLOC_TABLE_RESULT__);

  expect(result.error ?? null).toBeNull();
  expect(result.missingError).toBeTruthy();
  expect(String(result.missingError)).toContain("missing an allocation table");
  expect(result.okResult).toBeTruthy();
  expect(result.okResult.completedFence).toBe(2n);
  expect(result.okResult.presentCount).toBe(1n);
});
