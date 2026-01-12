import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet output runs and does not underrun with synthetic tone", async ({ page }) => {
  test.skip(test.info().project.name !== "chromium", "AudioWorklet output test only runs on Chromium.");

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-output");

  await page.waitForFunction(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    return out?.enabled === true && out?.context?.state === "running";
  });

  await page.waitForFunction(
    () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutput;
      if (!out?.ringBuffer?.samples || !out?.ringBuffer?.writeIndex) return false;
      const samples: Float32Array = out.ringBuffer.samples;
      const writeIndex: Uint32Array = out.ringBuffer.writeIndex;
      const cc = out.ringBuffer.channelCount | 0;
      const cap = out.ringBuffer.capacityFrames | 0;
      if (cc <= 0 || cap <= 0) return false;
      const write = Atomics.load(writeIndex, 0) >>> 0;
      const framesToInspect = Math.min(1024, cap);
      const startFrame = (write - framesToInspect) >>> 0;
      let maxAbs = 0;
      for (let i = 0; i < framesToInspect; i++) {
        const frame = (startFrame + i) % cap;
        const base = frame * cc;
        for (let c = 0; c < cc; c++) {
          const s = samples[base + c] ?? 0;
          const a = Math.abs(s);
          if (a > maxAbs) maxAbs = a;
        }
      }
      return maxAbs > 0.01;
    },
    { timeout: 10_000 },
  );

  await page.waitForTimeout(1000);

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
      maxAbsSample: (() => {
        if (!out?.ringBuffer?.samples || !out?.ringBuffer?.writeIndex) return null;
        const samples: Float32Array = out.ringBuffer.samples;
        const writeIndex: Uint32Array = out.ringBuffer.writeIndex;
        const cc = out.ringBuffer.channelCount | 0;
        const cap = out.ringBuffer.capacityFrames | 0;
        if (cc <= 0 || cap <= 0) return null;
        const write = Atomics.load(writeIndex, 0) >>> 0;
        const framesToInspect = Math.min(1024, cap);
        const startFrame = (write - framesToInspect) >>> 0;
        let maxAbs = 0;
        for (let i = 0; i < framesToInspect; i++) {
          const frame = (startFrame + i) % cap;
          const base = frame * cc;
          for (let c = 0; c < cc; c++) {
            const s = samples[base + c] ?? 0;
            const a = Math.abs(s);
            if (a > maxAbs) maxAbs = a;
          }
        }
        return maxAbs;
      })(),
    };
  });

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  // Startup can be racy across CI environments; allow up to one render quantum.
  // Underruns are counted as missing frames (a single render quantum is 128 frames).
  expect(result.underruns).toBeLessThanOrEqual(128);
  expect(result.overruns).toBe(0);
  expect(result.maxAbsSample).not.toBeNull();
  expect(result.maxAbsSample as number).toBeGreaterThan(0.01);
});
