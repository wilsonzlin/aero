import { expect, test } from "@playwright/test";

test("AudioWorklet output runs and does not underrun with synthetic tone", async ({ page }) => {
  test.skip(test.info().project.name !== "chromium", "AudioWorklet output test only runs on Chromium.");

  await page.goto("http://127.0.0.1:4173/", { waitUntil: "load" });

  await page.click("#init-audio-output");

  await page.waitForFunction(() => {
    // Exposed by `web/src/main.ts`.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    return out?.enabled === true && out?.context?.state === "running";
  });

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

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  expect(result.underruns).toBe(0);
  expect(result.overruns).toBe(0);
});
