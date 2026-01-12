import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("HDA capture consumes synthetic mic ring and DMA-writes PCM into guest RAM", async ({ page }) => {
  test.setTimeout(60_000);
  test.skip(test.info().project.name !== "chromium", "HDA capture test only runs on Chromium.");
  page.setDefaultTimeout(60_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-hda-capture-synthetic");

  await page.waitForFunction(() => {
    // Exposed by the repo-root Vite harness UI (`src/main.ts`).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (globalThis as any).__aeroAudioHdaCaptureSyntheticResult?.done === true;
  });

  const result = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (globalThis as any).__aeroAudioHdaCaptureSyntheticResult as Record<string, unknown> | undefined;
  });

  expect(result).toBeTruthy();
  expect(result?.ok).toBe(true);
  expect(result?.pcmNonZero).toBe(true);
  expect(result?.micReadDelta).toBeGreaterThan(0);
  expect(result?.micWriteDelta).toBeGreaterThan(0);
  // Startup can be racy in CI; allow some dropped samples but ensure it stays bounded.
  expect(result?.micDroppedDelta).toBeLessThanOrEqual(96_000);
});

