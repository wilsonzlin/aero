const path = require('node:path');
const { readFile } = require('node:fs/promises');

const { test } = require('@playwright/test');

const {
  PRIVATE_IMAGE_ID,
  PUBLIC_IMAGE_ID,
  startAppServer,
  startDiskGatewayStubServer,
} = require('../src/servers');

function fixturePath(name) {
  return path.join(__dirname, '..', 'fixtures', name);
}

test.describe('disk streaming COOP/COEP + Range + auth', () => {
  /** @type {{ origin: string, close: () => Promise<void> } | null} */
  let app = null;
  /** @type {{ origin: string, close: () => Promise<void> } | null} */
  let disk = null;

  let publicFixtureBytes;
  let privateFixtureBytes;

  test.beforeAll(async () => {
    publicFixtureBytes = await readFile(fixturePath('public.bin'));
    privateFixtureBytes = await readFile(fixturePath('private.bin'));

    disk = await startDiskGatewayStubServer({
      publicFixturePath: fixturePath('public.bin'),
      privateFixturePath: fixturePath('private.bin'),
    });
    app = await startAppServer();
  });

  test.afterAll(async () => {
    await Promise.allSettled([app?.close(), disk?.close()]);
  });

  test('public image: cross-origin Range fetch works under COEP and keeps isolation', async ({ page }) => {
    const start = 123;
    const endInclusive = 123 + 1024 - 1;
    const expectedBytes = Array.from(publicFixtureBytes.subarray(start, endInclusive + 1));

    await page.goto(`${app.origin}/?diskOrigin=${encodeURIComponent(disk.origin)}`);

    await page.evaluate(() => window.__diskStreamingE2E.assertCrossOriginIsolated());

    await page.evaluate(
      ({ imageId, start, endInclusive, expectedBytes }) =>
        window.__diskStreamingE2E.fetchPublicRange({
          imageId,
          start,
          endInclusive,
          expectedBytes,
        }),
      { imageId: PUBLIC_IMAGE_ID, start, endInclusive, expectedBytes },
    );

    await page.evaluate(() => window.__diskStreamingE2E.assertCrossOriginIsolated());
  });

  test('private image: requires token for Range fetch (lease → authorized Range)', async ({ page }) => {
    const start = 4096;
    const endInclusive = 4096 + 2048 - 1;
    const expectedBytes = Array.from(privateFixtureBytes.subarray(start, endInclusive + 1));

    await page.goto(`${app.origin}/?diskOrigin=${encodeURIComponent(disk.origin)}`);
    await page.evaluate(() => window.__diskStreamingE2E.assertCrossOriginIsolated());

    await page.evaluate(
      ({ imageId, start, endInclusive }) =>
        window.__diskStreamingE2E.fetchPrivateRangeExpectUnauthorized({
          imageId,
          start,
          endInclusive,
        }),
      { imageId: PRIVATE_IMAGE_ID, start, endInclusive },
    );

    const token = await page.evaluate(
      ({ imageId }) => window.__diskStreamingE2E.fetchLeaseToken({ imageId }),
      { imageId: PRIVATE_IMAGE_ID },
    );

    await page.evaluate(
      ({ imageId, token, start, endInclusive, expectedBytes }) =>
        window.__diskStreamingE2E.fetchPrivateRangeWithToken({
          imageId,
          token,
          start,
          endInclusive,
          expectedBytes,
        }),
      { imageId: PRIVATE_IMAGE_ID, token, start, endInclusive, expectedBytes },
    );

    await page.evaluate(() => window.__diskStreamingE2E.assertCrossOriginIsolated());
  });
});

