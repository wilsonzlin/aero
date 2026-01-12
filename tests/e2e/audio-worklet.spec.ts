import { expect, test } from "@playwright/test";

import { getAudioOutputMaxAbsSample, waitForAudioOutputNonSilent } from "./util/audio";

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

  await waitForAudioOutputNonSilent(page, "__aeroAudioOutput", { threshold: 0.01 });

  // Ignore any startup underruns while the AudioWorklet graph spins up; assert on the delta
  // over a steady-state window so cold CI runners remain stable.
  const steady0 = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    return {
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });
  expect(steady0).not.toBeNull();

  await page.waitForTimeout(1000);

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });

  const maxAbs = await getAudioOutputMaxAbsSample(page, "__aeroAudioOutput");

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  const deltaUnderrun = (((result.underruns as number) - (steady0!.underruns as number)) >>> 0) as number;
  const deltaOverrun = (((result.overruns as number) - (steady0!.overruns as number)) >>> 0) as number;
  expect(deltaUnderrun).toBeLessThanOrEqual(1024);
  expect(deltaOverrun).toBe(0);
  expect(maxAbs).not.toBeNull();
  expect(maxAbs as number).toBeGreaterThan(0.01);
});
