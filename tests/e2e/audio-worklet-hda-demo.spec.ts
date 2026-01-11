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

  await page.waitForTimeout(1000);

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaDemo;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
    };
  });

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  // Startup can be racy across CI environments; allow up to one render quantum.
  // Underruns are counted as missing frames (a single render quantum is 128 frames).
  expect(result.underruns).toBeLessThanOrEqual(128);
});
