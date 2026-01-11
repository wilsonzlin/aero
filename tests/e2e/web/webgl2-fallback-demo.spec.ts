import { expect, test } from "@playwright/test";

test('falls back to WebGL2 when WebGPU is unavailable', async ({ page }) => {
  await page.addInitScript(() => {
    // Playwright runs on Chromium where WebGPU may be enabled; force the fallback path.
    try {
      Object.defineProperty(navigator, 'gpu', { get: () => undefined, configurable: true });
      return;
    } catch {}

    // If `navigator.gpu` is non-configurable, break adapter creation instead.
    try {
      if (navigator.gpu) navigator.gpu.requestAdapter = async () => null;
    } catch {}
  });

  await page.goto('/web/webgl2_fallback_demo.html');

  await expect(page.locator('#backend')).toHaveText('WebGL2');

  await page.waitForFunction(() => window.__AERO_DEMO_FIRST_PRESENT === true);

  const pixel = await page.evaluate(() => {
    const canvas = document.querySelector('#frame');
    const scratch = document.createElement('canvas');
    scratch.width = canvas.width;
    scratch.height = canvas.height;
    const ctx = scratch.getContext('2d');
    ctx.drawImage(canvas, 0, 0);
    const x = Math.floor(scratch.width / 2);
    const y = Math.floor(scratch.height / 2);
    const { data } = ctx.getImageData(x, y, 1, 1);
    return Array.from(data);
  });

  // The demo draws a moving gradient; the center pixel should be non-black.
  expect(pixel[3]).toBeGreaterThan(0);
  expect(pixel[0] + pixel[1] + pixel[2]).toBeGreaterThan(0);
});
