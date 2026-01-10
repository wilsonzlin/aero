const path = require('node:path');
const { readFile } = require('node:fs/promises');

const { test } = require('@playwright/test');

const {
  PRIVATE_IMAGE_ID,
  PUBLIC_IMAGE_ID,
  startAppServer,
  startDiskGatewayServer,
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
    publicFixtureBytes = await readFile(fixturePath('win7.img'));
    privateFixtureBytes = await readFile(fixturePath('secret.img'));

    app = await startAppServer();
    try {
      disk = await startDiskGatewayServer({
        appOrigin: app.origin,
        publicFixturePath: fixturePath('win7.img'),
        privateFixturePath: fixturePath('secret.img'),
      });
    } catch (err) {
      // If disk-gateway fails to start, ensure the app server isn't left running
      // (Playwright may abort before `afterAll` runs).
      await app.close().catch(() => {});
      app = null;
      throw err;
    }
  });

  test.afterAll(async () => {
    await Promise.allSettled([app?.close(), disk?.close()]);
  });

  test('public image: cross-origin Range fetch works under COEP and keeps isolation', async ({ page }) => {
    const start = 123;
    const endInclusive = 123 + 1024 - 1;
    const expectedBytes = Array.from(publicFixtureBytes.subarray(start, endInclusive + 1));
    const expectedFileSize = publicFixtureBytes.length;

    await page.goto(`${app.origin}/?diskOrigin=${encodeURIComponent(disk.origin)}`);

    await page.evaluate(() => window.__diskStreamingE2E.assertCrossOriginIsolated());

    await page.evaluate(
      ({ imageId, start, endInclusive, expectedBytes, expectedFileSize }) =>
        window.__diskStreamingE2E.fetchPublicRange({
          imageId,
          start,
          endInclusive,
          expectedBytes,
          expectedFileSize,
        }),
      { imageId: PUBLIC_IMAGE_ID, start, endInclusive, expectedBytes, expectedFileSize },
    );

    await page.evaluate(() => window.__diskStreamingE2E.assertCrossOriginIsolated());
  });

  test('private image: requires token for Range fetch (lease → authorized Range)', async ({ page }) => {
    const start = 4096;
    const endInclusive = 4096 + 2048 - 1;
    const expectedBytes = Array.from(privateFixtureBytes.subarray(start, endInclusive + 1));
    const expectedFileSize = privateFixtureBytes.length;

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
      ({ imageId, token, start, endInclusive, expectedBytes, expectedFileSize }) =>
        window.__diskStreamingE2E.fetchPrivateRangeWithToken({
          imageId,
          token,
          start,
          endInclusive,
          expectedBytes,
          expectedFileSize,
        }),
      { imageId: PRIVATE_IMAGE_ID, token, start, endInclusive, expectedBytes, expectedFileSize },
    );

    // Optional disk-gateway mode: accept token via query-string (less secure, but used by some
    // deployments). This ensures both auth paths stay compatible with COEP/CORS/Range.
    await page.evaluate(
      ({ imageId, token, start, endInclusive, expectedBytes, expectedFileSize }) =>
        window.__diskStreamingE2E.fetchPrivateRangeWithQueryToken({
          imageId,
          token,
          start,
          endInclusive,
          expectedBytes,
          expectedFileSize,
        }),
      { imageId: PRIVATE_IMAGE_ID, token, start, endInclusive, expectedBytes, expectedFileSize },
    );

    await page.evaluate(() => window.__diskStreamingE2E.assertCrossOriginIsolated());
  });
});
