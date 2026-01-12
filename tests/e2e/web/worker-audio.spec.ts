import { expect, test } from "@playwright/test";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const thisDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(dirname(thisDir)));
const threadedWasmPath = join(repoRoot, "web", "src", "wasm", "pkg-threaded", "aero_wasm_bg.wasm");
const hasThreadedWasm = existsSync(threadedWasmPath);

if (process.env.CI && !hasThreadedWasm) {
  throw new Error(
    [
      "Threaded WASM package missing in CI.",
      "",
      `Expected: ${threadedWasmPath}`,
      "",
      "Build it with (from the repo root):",
      "  npm -w web run wasm:build",
    ].join("\n"),
  );
}

test("worker audio fills the shared ring buffer (no postMessage audio copies)", async ({ page }) => {
  await page.goto("/web/blank.html");

  // Runtime worker audio depends on the threaded WASM bundle being built into
  // `web/src/wasm/pkg-threaded`. When running Playwright in environments that
  // don't build WASM (e.g. `npx vite` without `npm run wasm:build`), skip instead
  // of hanging on an unfilled ring buffer.
  test.skip(!hasThreadedWasm, "Threaded WASM bundle is missing (run `cd web && npm run wasm:build:threaded`).");

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

      const log = document.getElementById("log");
      const coordinator = new WorkerCoordinator();
      window.__coordinator = coordinator;

      // Minimal config that keeps worker boot cheap.
      const config = {
        guestMemoryMiB: 256,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
        logLevel: "info",
      };

       try {
         coordinator.start(config);
         coordinator.getIoWorker()?.postMessage({ type: "setBootDisks", mounts: {}, hdd: null, cd: null });
       } catch (err) {
         log.textContent = err instanceof Error ? err.message : String(err);
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
  await page.waitForFunction(() => (window as any).__aeroAudioOutput?.getBufferLevelFrames?.() > 1024);

  const underruns0 = await page.evaluate(() => (window as any).__aeroAudioOutput.getUnderrunCount());
  await page.waitForTimeout(750);
  const underruns1 = await page.evaluate(() => (window as any).__aeroAudioOutput.getUnderrunCount());
  expect(underruns1).toBe(underruns0);
});
