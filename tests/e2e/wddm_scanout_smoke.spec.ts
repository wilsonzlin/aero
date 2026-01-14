import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

async function runWddmScanoutSmoke(page: Page, url: string) {
  await page.goto(url, { waitUntil: "load" });
  await waitForReady(page);

  return await page.evaluate(async () => {
    const scanout = await import("/web/src/ipc/scanout_state.ts");
    const api = (window as any).__aeroTest;
    if (!api) throw new Error("__aeroTest missing");
    if (api.error) throw new Error(api.error);
    if (api.pass !== true) {
      throw new Error(
        `wddm scanout mismatch: presented got=${api.hash ?? "none"} expected=${api.expectedHash ?? "none"} ` +
          `source got=${api.sourceHash ?? "none"} expected=${api.expectedSourceHash ?? "none"}`,
      );
    }

    const samples = api.samplePixels ? await api.samplePixels() : null;
    return {
      backend: api.backend ?? "unknown",
      hash: api.hash,
      expectedHash: api.expectedHash,
      sourceHash: api.sourceHash,
      expectedSourceHash: api.expectedSourceHash,
      samples,
      metrics: api.metrics ?? null,
      scanoutSourceWddm: scanout.SCANOUT_SOURCE_WDDM,
    };
  });
}

function assertWddmScanoutSmokeResult(result: any) {
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

  // Cursor redraw sanity: enabling the cursor must not clobber the active scanout output.
  expect(result.samples.cursor).toBeTruthy();
  expect(result.samples.cursor.x).toBe(2);
  expect(result.samples.cursor.y).toBe(2);
  expect(result.samples.cursor.pixel).toEqual([0, 0, 0, 255]);
  expect(result.samples.cursor.nearby).toEqual([255, 0, 0, 255]);

  // WDDM scanout must continue updating even when ScanoutState is unchanged (scanout memory can
  // change without a generation bump). The smoke harness mutates a pixel and expects the
  // presented output to reflect it.
  expect(result.samples.scanoutUpdate).toBeTruthy();
  expect(result.samples.scanoutUpdate.x).toBe(16);
  expect(result.samples.scanoutUpdate.y).toBe(16);
  expect(result.samples.scanoutUpdate.before).toEqual([255, 0, 0, 255]);
  expect(result.samples.scanoutUpdate.after).toEqual([0, 255, 0, 255]);

  // Validate the "X" byte in BGRX is ignored and alpha is forced to 255.
  for (const sample of [
    result.samples.source.topLeft,
    result.samples.source.topRight,
    result.samples.source.bottomLeft,
    result.samples.source.bottomRight,
  ]) {
    expect(sample[3]).toBe(255);
  }

  // Telemetry: verify scanout presentation is reported as WDDM scanout (not legacy framebuffer).
  expect(result.metrics).not.toBeNull();
  expect(result.metrics.outputSource).toBe("wddm_scanout");
  expect(result.metrics.scanout).toBeTruthy();
  expect(result.metrics.scanout.source).toBe(result.scanoutSourceWddm);
}

test("wddm scanout smoke: presents from guest RAM base_paddr (BGRX->RGBA, alpha=255)", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  const result = await runWddmScanoutSmoke(page, "/web/wddm-scanout-smoke.html");
  assertWddmScanoutSmokeResult(result);
});

test("wddm scanout smoke: presents from VRAM BAR1 base_paddr (BGRX->RGBA, alpha=255)", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  const result = await runWddmScanoutSmoke(page, "/web/wddm-scanout-smoke.html?backing=vram");
  assertWddmScanoutSmokeResult(result);
});
