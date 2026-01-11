import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import pixelmatch from 'pixelmatch';
import { PNG } from 'pngjs';
import type { TestInfo } from '@playwright/test';

export type RgbaImage = {
  width: number;
  height: number;
  rgba: Buffer;
};

export type GoldenDiffOptions = {
  /**
   * `pixelmatch` threshold (0 = strict, 0.1 = tolerant).
   *
   * Keep this low; the microtests are designed to avoid GPU-dependent antialiasing.
   */
  threshold?: number;
  /**
   * Maximum allowed number of mismatching pixels.
   */
  maxDiffPixels?: number;
};

function readPng(filePath: string): { width: number; height: number; rgba: Buffer } {
  const png = PNG.sync.read(fs.readFileSync(filePath));
  // png.data is a Buffer already.
  return { width: png.width, height: png.height, rgba: png.data };
}

function writePng(filePath: string, img: RgbaImage): void {
  const png = new PNG({ width: img.width, height: img.height });
  png.data = img.rgba;
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, PNG.sync.write(png));
}

function resolveGoldenPath(goldenName: string): string {
  // tests/e2e/playwright/utils -> tests/golden
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, "..", "..", "..", "golden", `${goldenName}.png`);
}

export async function expectRgbaToMatchGolden(
  testInfo: TestInfo,
  goldenName: string,
  actual: RgbaImage,
  options: GoldenDiffOptions = {}
): Promise<void> {
  const goldenPath = resolveGoldenPath(goldenName);
  if (!fs.existsSync(goldenPath)) {
    const actualPath = testInfo.outputPath(`${goldenName}.actual.png`);
    writePng(actualPath, actual);
    await testInfo.attach('actual', { path: actualPath, contentType: 'image/png' });
    throw new Error(
      `Missing golden image: ${goldenPath}\n` +
        `Wrote actual output to: ${actualPath}\n` +
        `Run \`npm run generate:goldens\` to (re)generate goldens (when deterministic).`
    );
  }

  const expected = readPng(goldenPath);
  const threshold = options.threshold ?? 0.1;
  const maxDiffPixels = options.maxDiffPixels ?? 0;

  if (expected.width !== actual.width || expected.height !== actual.height) {
    const actualPath = testInfo.outputPath(`${goldenName}.actual.png`);
    writePng(actualPath, actual);
    await testInfo.attach('actual', { path: actualPath, contentType: 'image/png' });
    await testInfo.attach('expected', { path: goldenPath, contentType: 'image/png' });
    throw new Error(
      `Golden size mismatch for "${goldenName}": expected ${expected.width}x${expected.height}, got ${actual.width}x${actual.height}`
    );
  }

  const diff = new PNG({ width: expected.width, height: expected.height });
  const diffPixels = pixelmatch(
    expected.rgba,
    actual.rgba,
    diff.data,
    expected.width,
    expected.height,
    { threshold }
  );

  if (diffPixels > maxDiffPixels) {
    const expectedOutPath = testInfo.outputPath(`${goldenName}.expected.png`);
    const actualOutPath = testInfo.outputPath(`${goldenName}.actual.png`);
    const diffOutPath = testInfo.outputPath(`${goldenName}.diff.png`);

    // Copy expected into output directory to keep artifacts self-contained.
    fs.copyFileSync(goldenPath, expectedOutPath);
    writePng(actualOutPath, actual);
    fs.writeFileSync(diffOutPath, PNG.sync.write(diff));

    await Promise.all([
      testInfo.attach('expected', { path: expectedOutPath, contentType: 'image/png' }),
      testInfo.attach('actual', { path: actualOutPath, contentType: 'image/png' }),
      testInfo.attach('diff', { path: diffOutPath, contentType: 'image/png' })
    ]);

    const totalPixels = expected.width * expected.height;
    const diffRatio = diffPixels / totalPixels;
    throw new Error(
      `Golden mismatch for "${goldenName}": ${diffPixels}/${totalPixels} pixels differ (${(
        diffRatio * 100
      ).toFixed(4)}%).\n` +
        `Artifacts written to: ${testInfo.outputDir}`
    );
  }
}
