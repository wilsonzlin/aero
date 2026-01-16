import { expect, test } from "@playwright/test";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const thisDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(dirname(thisDir)));
const threadedWasmBinaryRelease = join(repoRoot, "web", "src", "wasm", "pkg-threaded", "aero_wasm_bg.wasm");
const threadedWasmJsRelease = join(repoRoot, "web", "src", "wasm", "pkg-threaded", "aero_wasm.js");
const threadedWasmBinaryDev = join(repoRoot, "web", "src", "wasm", "pkg-threaded-dev", "aero_wasm_bg.wasm");
const threadedWasmJsDev = join(repoRoot, "web", "src", "wasm", "pkg-threaded-dev", "aero_wasm.js");
const hasThreadedWasmBundle =
  (existsSync(threadedWasmBinaryRelease) && existsSync(threadedWasmJsRelease)) ||
  (existsSync(threadedWasmBinaryDev) && existsSync(threadedWasmJsDev));

test("worker audio fills the shared ring buffer (no postMessage audio copies)", async ({ page }) => {
  await page.goto("/web/blank.html");

  // Runtime worker audio depends on the threaded WASM bundle being built into
  // `web/src/wasm/pkg-threaded`. When running Playwright in environments that
  // don't build WASM (e.g. `npx vite` without `npm run wasm:build`), skip instead
  // of hanging on an unfilled ring buffer.
  if (!hasThreadedWasmBundle) {
    const message = [
      "Threaded WASM bundle is missing (required for shared-memory worker audio).",
      "",
      "Expected one of:",
      `- ${threadedWasmBinaryRelease} (+ ${threadedWasmJsRelease})`,
      `- ${threadedWasmBinaryDev} (+ ${threadedWasmJsDev})`,
      "",
      "Build it with (from the repo root):",
      "  npm -w web run wasm:build",
    ].join("\n");
    if (process.env.CI) {
      throw new Error(message);
    }
    test.skip(true, message);
  }

  const support = await page.evaluate(() => {
    const AudioContextCtor =
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).AudioContext ?? (globalThis as any).webkitAudioContext;
    let wasmThreads = false;
    try {
      // eslint-disable-next-line no-new
      new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      wasmThreads = true;
    } catch {
      wasmThreads = false;
    }
    return {
      crossOriginIsolated: globalThis.crossOriginIsolated === true,
      sharedArrayBuffer: typeof SharedArrayBuffer !== "undefined",
      wasmThreads,
      audioWorklet: typeof AudioWorkletNode !== "undefined" && typeof AudioContextCtor !== "undefined",
    };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable in this Playwright environment.");
  test.skip(!support.audioWorklet, "AudioWorklet is unavailable in this Playwright environment.");

  await page.setContent(`
     <button id="start">Start audio</button>
     <pre id="log"></pre>
     <script type="module">
        import { WorkerCoordinator } from "/web/src/runtime/coordinator.ts";
        import { createAudioOutput } from "/web/src/platform/audio.ts";
        import { formatOneLineUtf8 } from "/web/src/text.ts";

        const log = document.getElementById("log");
        const MAX_ERROR_BYTES = 512;

        function formatOneLineError(err) {
          const msg = err instanceof Error ? err.message : err;
          return formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error";
        }
        const coordinator = new WorkerCoordinator();
        window.__coordinator = coordinator;

        // Minimal config that keeps worker boot cheap.
        const config = {
          // Keep guest memory small; the runtime reserves a large fixed region for
          // the WASM heap, so huge guest sizes significantly increase total
          // SharedArrayBuffer memory pressure and can cause audio underruns in
          // headless CI.
          guestMemoryMiB: 1,
          vramMiB: 0,
          enableWorkers: true,
          enableWebGPU: false,
          proxyUrl: null,
          activeDiskImage: null,
          logLevel: "info",
        };

        try {
          coordinator.start(config);
          coordinator.setBootDisks({}, null, null);
        } catch (err) {
          log.textContent = formatOneLineError(err);
        }

        document.getElementById("start").addEventListener("click", async () => {
          log.textContent = "";
          const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: "interactive" });
          window.__aeroAudioOutput = output;
          if (!output.enabled) {
            log.textContent = output.message;
            return;
          }

          coordinator.setAudioRingBuffer(
            output.ringBuffer.buffer,
            output.ringBuffer.capacityFrames,
            output.ringBuffer.channelCount,
            output.context.sampleRate,
          );

          await output.resume();
          log.textContent = "started";
        });
    </script>
  `);

  await page.click("#start");

  await page.waitForFunction(() => (window as any).__aeroAudioOutput?.enabled === true);
  await page.waitForFunction(() => (window as any).__aeroAudioOutput?.context?.state === "running");

  // Wait for the worker to write more than the AudioWorklet startup prefill.
  await page.waitForFunction(() => (window as any).__aeroAudioOutput?.getBufferLevelFrames?.() > 2048);

  // Ignore any startup underruns while the AudioWorklet graph spins up and the
  // worker begins producing. Assert on the delta over a steady-state window so
  // cold runners remain stable.
  await page.waitForTimeout(1000);
  const metrics0 = await page.evaluate(() => (window as any).__aeroAudioOutput.getMetrics());
  await page.waitForTimeout(1000);
  const metrics1 = await page.evaluate(() => (window as any).__aeroAudioOutput.getMetrics());
  const deltaUnderrun = ((metrics1.underrunCount - metrics0.underrunCount) >>> 0) as number;
  const deltaOverrun = ((metrics1.overrunCount - metrics0.overrunCount) >>> 0) as number;
  expect(metrics1.bufferLevelFrames).toBeGreaterThan(0);
  expect(deltaOverrun).toBe(0);
  // This is a smoke test for SharedArrayBuffer-based audio, not a strict
  // real-time audio quality benchmark. Headless CI can still drop some quanta.
  expect(deltaUnderrun).toBeLessThanOrEqual(16_384);
});
