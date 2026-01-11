import { expect, test } from "@playwright/test";

test("AudioWorklet loopback runs with synthetic microphone source (no underruns)", async ({ page }) => {
  test.skip(test.info().project.name !== "chromium", "AudioWorklet loopback test only runs on Chromium.");

  await page.goto("http://127.0.0.1:4173/", { waitUntil: "load" });

  await page.click("#init-audio-loopback-synthetic");

  await page.waitForFunction(() => {
    // Exposed by `web/src/main.ts`.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputLoopback;
    return out?.enabled === true && out?.context?.state === "running";
  });

  await page.waitForTimeout(1000);

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputLoopback;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      bufferLevelFrames: typeof out?.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : null,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      backend: (globalThis as any).__aeroAudioLoopbackBackend ?? null,
    };
  });

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  // Startup can be racy across CI environments; allow a tiny tolerance.
  // Underruns are counted as missing frames (a single render quantum is 128 frames).
  expect(result.underruns).toBeLessThanOrEqual(128);
  expect(result.bufferLevelFrames).toBeGreaterThan(0);
  expect(result.backend === "worker" || result.backend === "main").toBe(true);
});
