import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("gpu worker smoke: renders pattern and returns screenshot hash", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/gpu-worker-smoke.html", { waitUntil: "load" });
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
      samples,
    };
  });

  expect(result.backend === "webgpu" || result.backend === "webgl2_raw").toBe(true);
  expect(result.hash).toBe(result.expectedHash);

  expect(result.samples).not.toBeNull();
  expect(result.samples.width).toBe(64);
  expect(result.samples.height).toBe(64);

  expect(result.samples.topLeft).toEqual([255, 0, 0, 255]);
  expect(result.samples.topRight).toEqual([0, 255, 0, 255]);
  expect(result.samples.bottomLeft).toEqual([0, 0, 255, 255]);
  expect(result.samples.bottomRight).toEqual([255, 255, 255, 255]);
});

test("gpu worker smoke: disableWebGpu forces WebGL2 fallback", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/gpu-worker-smoke.html?disableWebGpu=1", { waitUntil: "load" });
  await waitForReady(page);

  const result = await page.evaluate(() => {
    const api = (window as any).__aeroTest;
    if (!api) throw new Error("__aeroTest missing");
    if (api.error) throw new Error(api.error);
    return {
      backend: api.backend ?? "unknown",
      fallback: api.fallback ?? null,
      pass: api.pass,
    };
  });

  expect(result.backend).toBe("webgl2_raw");
  expect(result.fallback).not.toBeNull();
  expect(result.fallback.from).toBe("webgpu");
  expect(result.fallback.to).toBe("webgl2_raw");
  expect(result.pass).toBe(true);
});
