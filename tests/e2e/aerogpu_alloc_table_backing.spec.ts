import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("aerogpu alloc_table backing: GPU worker uploads from shared guest RAM via RESOURCE_DIRTY_RANGE", async ({ page }) => {
  await page.goto("http://127.0.0.1:5173/web/aerogpu-alloc-table-smoke.html", { waitUntil: "load" });
  await waitForReady(page);

  const result = await page.evaluate(() => (window as any).__aeroTest);
  expect(result).toBeTruthy();
  if (!result || typeof result !== "object") {
    throw new Error("Missing __aeroTest result");
  }
  if ((result as any).error) {
    throw new Error(String((result as any).error));
  }

  expect((result as any).pass).toBe(true);
  expect((result as any).width).toBe(3);
  expect((result as any).height).toBe(2);

  const samples = (result as any).samples;
  expect(samples).toBeTruthy();
  expect(samples.p00).toEqual([255, 0, 0, 255]);
  expect(samples.p10).toEqual([0, 255, 0, 255]);
  expect(samples.p20).toEqual([0, 0, 255, 255]);
  // Second row stays zeroed because we didn't dirty it; this validates row_pitch_bytes
  // padding does not bleed into packed RGBA8 presentation.
  expect(samples.p01).toEqual([0, 0, 0, 0]);
});

