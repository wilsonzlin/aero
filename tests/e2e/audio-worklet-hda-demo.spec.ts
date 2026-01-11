import { expect, test } from "@playwright/test";

test("AudioWorklet output runs and does not underrun with HDA DMA demo", async ({ page }) => {
  test.skip(test.info().project.name !== "chromium", "AudioWorklet output test only runs on Chromium.");

  await page.goto("http://127.0.0.1:4173/", { waitUntil: "load" });

  await page.click("#init-audio-output-hda-demo");

  await page.waitForFunction(() => {
    // Exposed by `web/src/main.ts`.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaDemo;
    return out?.enabled === true && out?.context?.state === "running";
  });

  const initialWrite = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaDemo;
    return Atomics.load(out.ringBuffer.header, 1) >>> 0;
  });

  await page.waitForTimeout(750);

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaDemo;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const backend = (globalThis as any).__aeroAudioToneBackend;
    const read = Atomics.load(out.ringBuffer.header, 0) >>> 0;
    const write = Atomics.load(out.ringBuffer.header, 1) >>> 0;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      backend,
      read,
      write,
      bufferLevelFrames: typeof out?.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : null,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
    };
  });

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  expect(result.backend).toBe("wasm-hda");
  expect(((result.write - initialWrite) >>> 0)).toBeGreaterThan(0);
  expect(result.bufferLevelFrames).not.toBeNull();
  expect(result.bufferLevelFrames as number).toBeGreaterThan(0);
  // Startup can be racy across CI environments; allow up to one render quantum.
  // Underruns are counted as missing frames (a single render quantum is 128 frames).
  expect(result.underruns).toBeLessThanOrEqual(128);
}); 
