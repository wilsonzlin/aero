import { expect, test } from "@playwright/test";

import { getAudioOutputMaxAbsSample, waitForAudioOutputNonSilent } from "./util/audio";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet output runs and receives frames from IO-worker HDA PCI/MMIO device", async ({ page }) => {
  // Full worker runtime + IO-worker WASM init can be slower than the standalone HDA demo,
  // especially on cold CI runners without a cached compilation artifact.
  test.setTimeout(90_000);
  test.skip(test.info().project.name !== "chromium", "AudioWorklet output test only runs on Chromium.");

  // The full worker HDA path involves instantiating a large WASM module in the IO worker
  // (often uncached in CI), so tolerate longer startup times than Playwright's 30s default.
  page.setDefaultTimeout(90_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-hda-pci-device");

  await page.waitForFunction(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    return out?.enabled === true && out?.context?.state === "running";
  });

  const initialIndices = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    const ring = out.ringBuffer as {
      readIndex: Uint32Array;
      writeIndex: Uint32Array;
    };
    return {
      read: Atomics.load(ring.readIndex, 0) >>> 0,
      write: Atomics.load(ring.writeIndex, 0) >>> 0,
    };
  });
  const initialRead = initialIndices.read;
  const initialWrite = initialIndices.write;

  // Sanity check: ensure the AudioWorklet is actually consuming frames.
  await page.waitForFunction(
    (initialRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      const ring = out.ringBuffer as { readIndex: Uint32Array };
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      return ((read - (initialRead as number)) >>> 0) > 0;
    },
    initialRead,
    { timeout: 10_000 },
  );

  // The IO-worker HDA device should be producing into the ring buffer.
  await page.waitForFunction(
    (initialWrite) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (initialWrite as number)) >>> 0) > 0;
    },
    initialWrite,
    // Allow enough time for IO-worker WASM init + PCI enumeration/programming in CI.
    { timeout: 45_000 },
  );

  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputHdaPciDevice", { threshold: 0.01 });

  await page.waitForTimeout(1000);

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const backend = (globalThis as any).__aeroAudioToneBackend;
    const ring = out.ringBuffer as { readIndex: Uint32Array; writeIndex: Uint32Array };
    const read = Atomics.load(ring.readIndex, 0) >>> 0;
    const write = Atomics.load(ring.writeIndex, 0) >>> 0;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      backend,
      read,
      write,
      bufferLevelFrames: typeof out?.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : null,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });

  const maxAbs = await getAudioOutputMaxAbsSample(page, "__aeroAudioOutputHdaPciDevice");

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  expect(result.backend).toBe("io-worker-hda-pci");
  expect(((result.write - initialWrite) >>> 0)).toBeGreaterThan(0);
  expect(result.bufferLevelFrames).not.toBeNull();
  expect(result.bufferLevelFrames as number).toBeGreaterThan(0);
  // Startup is still subject to scheduler variance in CI; allow up to one render quantum worth of underrun.
  expect(result.underruns).toBeLessThanOrEqual(128);
  expect(result.overruns).toBe(0);
  // Confirm we are not just advancing indices with silence.
  expect(maxAbs).not.toBeNull();
  expect(maxAbs as number).toBeGreaterThan(0.01);
});
