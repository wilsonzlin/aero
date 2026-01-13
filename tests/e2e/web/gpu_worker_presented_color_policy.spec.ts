import { expect, test, type Page } from "@playwright/test";

import { isWebGPURequired } from "../util/env";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

async function runBackend(page: Page, backend: string) {
  await page.goto(`/web/gpu-worker-color-policy.html?backend=${encodeURIComponent(backend)}`, { waitUntil: "load" });
  await waitForReady(page);

  return await page.evaluate(async () => {
    const api = (window as any).__aeroTest;
    if (!api) throw new Error("__aeroTest missing");
    return {
      backend: api.backend ?? "unknown",
      error: api.error ?? null,
      hash: api.hash ?? null,
      samples: api.samplePixels ? await api.samplePixels() : null,
    };
  });
}

test.describe("gpu worker presented color policy", () => {
  test("webgl2_raw matches expected presented output (sRGB + opaque)", async ({ page, browserName }) => {
    test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

    const result = await runBackend(page, "webgl2_raw");

    expect(result.error).toBeNull();
    expect(result.backend).toBe("webgl2_raw");

    const samples = result.samples;
    expect(samples).not.toBeNull();
    expect(samples.width).toBe(128);
    expect(samples.height).toBe(128);
    expect(samples.topLeft).toEqual([255, 0, 0, 255]);
    expect(samples.topRight).toEqual([0, 255, 0, 255]);
    expect(samples.bottomLeft).toEqual([0, 0, 255, 255]);
    expect(samples.bottomRight).toEqual([255, 255, 255, 255]);

    // Alpha policy: force opaque even when the source framebuffer has alpha=0.
    expect(samples.alphaZero).toEqual(samples.alphaZeroExpected);

    // Gamma policy: ensure we are sRGB-encoding (not outputting linear bytes or double-encoding).
    // Use a small tolerance to avoid per-backend float rounding differences.
    const actual = samples.midGray;
    const expected = samples.midGrayExpected;
    expect(actual[3]).toBe(255);
    for (let c = 0; c < 3; c++) {
      expect(Math.abs(actual[c]! - expected[c]!)).toBeLessThanOrEqual(2);
    }
  });

  test("webgpu matches expected presented output and matches webgl2_raw @webgpu", async ({ page, browserName }) => {
    test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

    const webgl2 = await runBackend(page, "webgl2_raw");
    expect(webgl2.error).toBeNull();
    expect(webgl2.backend).toBe("webgl2_raw");

    const webgpu = await runBackend(page, "webgpu");
    if (webgpu.error) {
      const message = String(webgpu.error);
      if (!isWebGPURequired() && /webgpu|navigator\\.gpu|adapter|device/i.test(message)) {
        test.skip(true, `WebGPU is not available/usable in this Playwright environment: ${message}`);
      }
    }

    expect(webgpu.error).toBeNull();
    expect(webgpu.backend).toBe("webgpu");
    expect(webgpu.hash).toBe(webgl2.hash);

    const samples = webgpu.samples;
    expect(samples).not.toBeNull();
    expect(samples.topLeft).toEqual([255, 0, 0, 255]);
    expect(samples.topRight).toEqual([0, 255, 0, 255]);
    expect(samples.bottomLeft).toEqual([0, 0, 255, 255]);
    expect(samples.bottomRight).toEqual([255, 255, 255, 255]);
    expect(samples.alphaZero).toEqual(samples.alphaZeroExpected);

    const actual = samples.midGray;
    const expected = samples.midGrayExpected;
    expect(actual[3]).toBe(255);
    for (let c = 0; c < 3; c++) {
      expect(Math.abs(actual[c]! - expected[c]!)).toBeLessThanOrEqual(2);
    }
  });

  test("webgl2_wgpu matches expected presented output (sRGB + opaque)", async ({ page, browserName }) => {
    test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

    const result = await runBackend(page, "webgl2_wgpu");

    expect(result.error).toBeNull();
    expect(result.backend).toBe("webgl2_wgpu");

    const samples = result.samples;
    expect(samples).not.toBeNull();
    expect(samples.width).toBe(128);
    expect(samples.height).toBe(128);
    expect(samples.topLeft).toEqual([255, 0, 0, 255]);
    expect(samples.topRight).toEqual([0, 255, 0, 255]);
    expect(samples.bottomLeft).toEqual([0, 0, 255, 255]);
    expect(samples.bottomRight).toEqual([255, 255, 255, 255]);
    expect(samples.alphaZero).toEqual(samples.alphaZeroExpected);

    const actual = samples.midGray;
    const expected = samples.midGrayExpected;
    expect(actual[3]).toBe(255);
    for (let c = 0; c < 3; c++) {
      expect(Math.abs(actual[c]! - expected[c]!)).toBeLessThanOrEqual(2);
    }
  });
});
