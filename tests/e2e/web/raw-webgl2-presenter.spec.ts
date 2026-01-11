import { expect, test } from '@playwright/test';

test('forceBackend=webgl2_raw can present and read back screenshot', async ({ page }) => {
  await page.goto('/web/raw_webgl2_presenter_test.html');

  const width = 64;
  const height = 64;
  const dpr = 1;

  // Expected hash for the RGBA pattern below (SHA-256 over raw bytes).
  const expectedHash = '0ede29c88978d2dfc76557e5b7c8d2114aaf78e2278aa5c1348da7726f8fdd1f';

  const inputHash = await page.evaluate(
    async ({ width, height, dpr, expectedHash }) => {
      if (!window.__aeroTest) throw new Error('test harness missing');
      await window.__aeroTest.init({
        width,
        height,
        dpr,
        forceBackend: 'webgl2_raw',
        scaleMode: 'stretch',
      });

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

      const digest = await crypto.subtle.digest('SHA-256', frame);
      const hash = Array.from(new Uint8Array(digest))
        .map((b) => b.toString(16).padStart(2, '0'))
        .join('');
      if (hash !== expectedHash) {
        throw new Error(`Unexpected test pattern hash: got ${hash} expected ${expectedHash}`);
      }

      window.__aeroTest.present(frame, width * 4);
      return hash;
    },
    { width, height, dpr, expectedHash },
  );
  expect(inputHash).toBe(expectedHash);

  const screenshot = await page.evaluate(async () => {
    if (!window.__aeroTest) throw new Error('test harness missing');
    const shot = await window.__aeroTest.screenshot();
    const digest = await crypto.subtle.digest('SHA-256', shot.pixels);
    const hash = Array.from(new Uint8Array(digest))
      .map((b) => b.toString(16).padStart(2, '0'))
      .join('');
    return { width: shot.width, height: shot.height, hash };
  });

  expect(screenshot.width).toBe(width);
  expect(screenshot.height).toBe(height);
  expect(screenshot.hash).toBe(expectedHash);
});
