import { expect, test } from "@playwright/test";

import { getAudioOutputMaxAbsSample, waitForAudioOutputNonSilent } from "./util/audio";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet output runs and does not underrun with virtio-snd demo", async ({ page }) => {
  test.setTimeout(60_000);
  test.skip(test.info().project.name !== "chromium", "AudioWorklet output test only runs on Chromium.");

  page.setDefaultTimeout(60_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-virtio-snd-demo");

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputVirtioSndDemo;
    return out?.enabled === true && out?.context?.state === "running";
  });

  const initialIndices = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputVirtioSndDemo;
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

  await page.waitForFunction(
    (initialRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputVirtioSndDemo;
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
      const out = (globalThis as any).__aeroAudioOutputVirtioSndDemo;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (initialWrite as number)) >>> 0) > 0;
    },
    initialWrite,
    { timeout: 45_000 },
  );

  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputVirtioSndDemo", { threshold: 0.01 });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const stats = (globalThis as any).__aeroAudioVirtioSndDemoStats;
    return stats && typeof stats.totalFramesWritten === "number" && stats.totalFramesWritten > 0;
  });

  const steady0 = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputVirtioSndDemo;
    return {
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });
  expect(steady0).not.toBeNull();

  await page.waitForTimeout(1000);

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputVirtioSndDemo;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const backend = (globalThis as any).__aeroAudioToneBackend;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const demoStats = (globalThis as any).__aeroAudioVirtioSndDemoStats;
    const ring = out.ringBuffer as { readIndex: Uint32Array; writeIndex: Uint32Array };
    const read = Atomics.load(ring.readIndex, 0) >>> 0;
    const write = Atomics.load(ring.writeIndex, 0) >>> 0;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      backend,
      totalWritten: typeof demoStats?.totalFramesWritten === "number" ? demoStats.totalFramesWritten : null,
      read,
      write,
      bufferLevelFrames: typeof out?.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : null,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });

  const maxAbs = await getAudioOutputMaxAbsSample(page, "__aeroAudioOutputVirtioSndDemo");

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  expect(result.backend).toBe("wasm-virtio-snd");
  expect(((result.write - initialWrite) >>> 0)).toBeGreaterThan(0);
  expect(result.totalWritten).not.toBeNull();
  expect(result.totalWritten as number).toBeGreaterThan(0);
  expect(result.bufferLevelFrames).not.toBeNull();
  expect(result.bufferLevelFrames as number).toBeGreaterThan(0);

  const deltaUnderrun = (((result.underruns as number) - (steady0!.underruns as number)) >>> 0) as number;
  const deltaOverrun = (((result.overruns as number) - (steady0!.overruns as number)) >>> 0) as number;
  expect(deltaUnderrun).toBeLessThanOrEqual(1024);
  expect(deltaOverrun).toBe(0);
  expect(maxAbs).not.toBeNull();
  expect(maxAbs as number).toBeGreaterThan(0.01);
});

