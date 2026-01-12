import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("aerogpu alloc_table backing: GPU worker uploads from shared guest RAM via RESOURCE_DIRTY_RANGE", async ({ page }) => {
  await page.goto("/web/aerogpu-alloc-table-smoke.html", { waitUntil: "load" });

  const support = await page.evaluate(() => {
    let wasmThreads = false;
    try {
      // eslint-disable-next-line no-new
      new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      wasmThreads = true;
    } catch {
      wasmThreads = false;
    }
    return {
      crossOriginIsolated: globalThis.crossOriginIsolated === true,
      sharedArrayBuffer: typeof SharedArrayBuffer !== "undefined",
      atomics: typeof Atomics !== "undefined",
      wasmThreads,
    };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics || !support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");
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
