import { expect, test } from "@playwright/test";

import { getAudioOutputMaxAbsSample, waitForAudioOutputNonSilent } from "./util/audio";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet output runs and does not underrun with HDA DMA demo", async ({ page }) => {
  test.setTimeout(60_000);
  test.skip(test.info().project.name !== "chromium", "AudioWorklet output test only runs on Chromium.");

  // The HDA demo boots a dedicated worker that loads and instantiates a large WASM module.
  // When Chromium doesn't have a cached compilation artifact yet (common in CI), this can take
  // longer than Playwright's default 30s timeout.
  page.setDefaultTimeout(60_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-hda-demo");

  await page.waitForFunction(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaDemo;
    return out?.enabled === true && out?.context?.state === "running";
  });

  const initialIndices = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaDemo;
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

  // Sanity check: ensure the AudioWorklet is actually consuming from the ring.
  await page.waitForFunction(
    (initialRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaDemo;
      const ring = out.ringBuffer as { readIndex: Uint32Array };
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      return ((read - (initialRead as number)) >>> 0) > 0;
    },
    initialRead,
    { timeout: 10_000 },
  );

  await page.waitForFunction(
    (initialWrite) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaDemo;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (initialWrite as number)) >>> 0) > 0;
    },
    initialWrite,
    // Allow enough time for WASM compilation + worker startup in CI.
    { timeout: 45_000 },
  );

  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputHdaDemo", { threshold: 0.01 });

  await page.waitForFunction(() => {
    // Exposed by the audio demo UI (updated from the CPU worker).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const stats = (globalThis as any).__aeroAudioHdaDemoStats;
    return stats && typeof stats.totalFramesWritten === "number" && stats.totalFramesWritten > 0;
  });

  // Let the system run for a bit so we catch sustained underruns (not just “it started once”).
  await page.waitForTimeout(1000);

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaDemo;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const backend = (globalThis as any).__aeroAudioToneBackend;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const hdaStats = (globalThis as any).__aeroAudioHdaDemoStats;
    const ring = out.ringBuffer as { readIndex: Uint32Array; writeIndex: Uint32Array };
    const read = Atomics.load(ring.readIndex, 0) >>> 0;
    const write = Atomics.load(ring.writeIndex, 0) >>> 0;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      backend,
      hdaTotalWritten: typeof hdaStats?.totalFramesWritten === "number" ? hdaStats.totalFramesWritten : null,
      read,
      write,
      bufferLevelFrames: typeof out?.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : null,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });

  const maxAbs = await getAudioOutputMaxAbsSample(page, "__aeroAudioOutputHdaDemo");

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  expect(result.backend).toBe("wasm-hda");
  expect(((result.write - initialWrite) >>> 0)).toBeGreaterThan(0);
  expect(result.hdaTotalWritten).not.toBeNull();
  expect(result.hdaTotalWritten as number).toBeGreaterThan(0);
  expect(result.bufferLevelFrames).not.toBeNull();
  expect(result.bufferLevelFrames as number).toBeGreaterThan(0);
  // Startup can be racy across CI environments; allow up to one render quantum.
  // Underruns are counted as missing frames (a single render quantum is 128 frames).
  expect(result.underruns).toBeLessThanOrEqual(128);
  expect(result.overruns).toBe(0);
  expect(maxAbs).not.toBeNull();
  expect(maxAbs as number).toBeGreaterThan(0.01);
});
