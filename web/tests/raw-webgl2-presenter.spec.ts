import { expect, test } from '@playwright/test';
import { createHash } from 'node:crypto';

test('forceBackend=webgl2_raw can present and read back screenshot', async ({ page }) => {
  await page.goto('/raw_webgl2_presenter_test.html');

  const width = 64;
  const height = 64;
  const dpr = 1;

  await page.evaluate(
    async ({ width, height, dpr }) => {
      if (!window.__aeroTest) throw new Error('test harness missing');
      await window.__aeroTest.init({
        width,
        height,
        dpr,
        forceBackend: 'webgl2_raw',
        scaleMode: 'stretch',
      });
    },
    { width, height, dpr },
  );

  // Generate a deterministic RGBA8 test pattern (top-left origin).
  const frame = new Uint8Array(width * height * 4);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      frame[i + 0] = x & 0xff;
      frame[i + 1] = y & 0xff;
      frame[i + 2] = (x ^ y) & 0xff;
      frame[i + 3] = 0xff;
    }
  }

  // Expected hash for the pattern above (SHA-256 over raw RGBA bytes).
  const expectedHash = '0ede29c88978d2dfc76557e5b7c8d2114aaf78e2278aa5c1348da7726f8fdd1f';
  const inputHash = createHash('sha256').update(frame).digest('hex');
  expect(inputHash).toBe(expectedHash);

  await page.evaluate(
    ({ bytes, stride }) => {
      if (!window.__aeroTest) throw new Error('test harness missing');
      window.__aeroTest.present(new Uint8Array(bytes), stride);
    },
    { bytes: frame.buffer, stride: width * 4 },
  );

  const screenshot = await page.evaluate(async () => {
    if (!window.__aeroTest) throw new Error('test harness missing');
    const shot = await window.__aeroTest.screenshot();
    return { width: shot.width, height: shot.height, bytes: shot.pixels.buffer };
  });

  expect(screenshot.width).toBe(width);
  expect(screenshot.height).toBe(height);

  const pixels = new Uint8Array(screenshot.bytes);
  const screenshotHash = createHash('sha256').update(pixels).digest('hex');
  expect(screenshotHash).toBe(expectedHash);
});
