import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("wddm scanout smoke: presents from guest RAM base_paddr (BGRX->RGBA, alpha=255)", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/wddm-scanout-smoke.html", { waitUntil: "load" });
  await waitForReady(page);

  const result = await page.evaluate(async () => {
    const api = (window as any).__aeroTest;
    if (!api) throw new Error("__aeroTest missing");
    if (api.error) throw new Error(api.error);
    if (api.pass !== true) {
      throw new Error(`hash mismatch: got=${api.hash ?? "none"} expected=${api.expectedHash ?? "none"}`);
    }

    const samples = api.samplePixels ? await api.samplePixels() : null;
    return {
      backend: api.backend ?? "unknown",
      hash: api.hash,
      expectedHash: api.expectedHash,
      sourceHash: api.sourceHash,
      expectedSourceHash: api.expectedSourceHash,
      samples,
    };
  });

  expect(result.backend).toBe("webgl2_raw");
  expect(result.hash).toBe(result.expectedHash);
  expect(result.sourceHash).toBe(result.expectedSourceHash);
  expect(result.samples).not.toBeNull();
  expect(result.samples.source.width).toBe(64);
  expect(result.samples.source.height).toBe(64);
  expect(result.samples.presented.width).toBe(64);
  expect(result.samples.presented.height).toBe(64);

  // Source framebuffer samples (validates BGRX->RGBA swizzle + alpha policy).
  expect(result.samples.source.topLeft).toEqual([255, 0, 0, 255]);
  expect(result.samples.source.topRight).toEqual([0, 255, 0, 255]);
  expect(result.samples.source.bottomLeft).toEqual([0, 0, 255, 255]);
  expect(result.samples.source.bottomRight).toEqual([255, 255, 255, 255]);

  // Presented output samples (validates that the scanout path is actually presented).
  expect(result.samples.presented.topLeft).toEqual([255, 0, 0, 255]);
  expect(result.samples.presented.topRight).toEqual([0, 255, 0, 255]);
  expect(result.samples.presented.bottomLeft).toEqual([0, 0, 255, 255]);
  expect(result.samples.presented.bottomRight).toEqual([255, 255, 255, 255]);

  // Validate the "X" byte in BGRX is ignored and alpha is forced to 255.
  for (const sample of [
    result.samples.source.topLeft,
    result.samples.source.topRight,
    result.samples.source.bottomLeft,
    result.samples.source.bottomRight,
  ]) {
    expect(sample[3]).toBe(255);
  }
});
