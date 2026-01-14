import { expect, test, type Page } from "@playwright/test";

import { isWebGPURequired } from "./util/env";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

async function runBackend(page: Page, backend: string) {
  await page.goto(`/web/gpu-worker-presented-cursor-overlay.html?backend=${encodeURIComponent(backend)}`, {
    waitUntil: "load",
  });
  await waitForReady(page);

  return await page.evaluate(() => {
    return (window as any).__aeroTest;
  });
}

test.describe("gpu worker presented cursor overlay", () => {
  test("webgl2_raw blends cursor in linear then encodes to sRGB", async ({ page, browserName }) => {
    test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

    const result = await runBackend(page, "webgl2_raw");
    expect(result?.error ?? null).toBeNull();
    expect(result.backend).toBe("webgl2_raw");

    expect(result.sampleNoCursor).toEqual(result.expectedNoCursor);

    const sample: number[] = result.sample;
    const expected: number[] = result.expected;
    expect(sample[3]).toBe(255);
    expect(expected[3]).toBe(255);
    for (let c = 0; c < 3; c++) {
      expect(Math.abs(sample[c]! - expected[c]!)).toBeLessThanOrEqual(2);
    }
  });

  test("webgpu blends cursor in linear then encodes to sRGB @webgpu", async ({ page, browserName }) => {
    test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

    const result = await runBackend(page, "webgpu");
    if (result?.error) {
      const message = String(result.error);
      if (!isWebGPURequired() && /webgpu|navigator\\.gpu|adapter|device/i.test(message)) {
        test.skip(true, `WebGPU is not available/usable in this Playwright environment: ${message}`);
      }
    }

    expect(result?.error ?? null).toBeNull();
    expect(result.backend).toBe("webgpu");

    expect(result.sampleNoCursor).toEqual(result.expectedNoCursor);

    const sample: number[] = result.sample;
    const expected: number[] = result.expected;
    expect(sample[3]).toBe(255);
    expect(expected[3]).toBe(255);
    for (let c = 0; c < 3; c++) {
      expect(Math.abs(sample[c]! - expected[c]!)).toBeLessThanOrEqual(2);
    }
  });
});
