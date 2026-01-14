import crypto from 'node:crypto';

import { expect, test } from '@playwright/test';

function sha256Hex(buf: Buffer): string {
  return crypto.createHash('sha256').update(buf).digest('hex');
}

test('aero-gpu-wasm upload_rgba8888_dirty_rects only uploads dirty region', async ({ page }, testInfo) => {
  // The wgpu WebGL2 backend used by `aero-gpu-wasm` can be flaky in some browsers/headless
  // configurations; keep this test on the default Chromium project.
  test.skip(testInfo.project.name !== 'chromium', 'wgpu WebGL2 upload test only runs in Chromium');

  // Use a minimal same-origin page so we can dynamic-import Vite TS modules.
  await page.goto('/web/src/pages/blank.html', { waitUntil: 'load' });

  const caps = await page.evaluate(() => {
    return {
      offscreen: typeof OffscreenCanvas !== 'undefined',
      transfer:
        typeof HTMLCanvasElement !== 'undefined' &&
        typeof (HTMLCanvasElement.prototype as any).transferControlToOffscreen === 'function',
    };
  });
  test.skip(!caps.offscreen, 'OffscreenCanvas is unavailable in this browser');
  test.skip(!caps.transfer, 'HTMLCanvasElement.transferControlToOffscreen() is unavailable in this browser');

  const result = await page.evaluate(async () => {
    // Chunked btoa to avoid call stack limits.
    function uint8ToBase64(u8: Uint8Array): string {
      const chunkSize = 0x8000;
      let binary = '';
      for (let i = 0; i < u8.length; i += chunkSize) {
        const chunk = u8.subarray(i, i + chunkSize);
        binary += String.fromCharCode.apply(null, Array.from(chunk));
      }
      return btoa(binary);
    }

    const wasm = await import('/web/src/wasm/aero-gpu.ts');
    await wasm.default();

    const beforeStats = (wasm.get_gpu_stats?.() as any) ?? {};

    const width = 16;
    const height = 16;
    const htmlCanvas = document.createElement('canvas');
    htmlCanvas.width = width;
    htmlCanvas.height = height;
    document.body.appendChild(htmlCanvas);
    const canvas = htmlCanvas.transferControlToOffscreen();

    // Clear stale state from a previous init (best-effort).
    try {
      wasm.destroy_gpu();
    } catch {
      // Ignore.
    }

    await wasm.init_gpu(canvas, width, height, 1, {
      preferWebGpu: false,
      disableWebGpu: true,
    });

    const backend = wasm.backend_kind();
    const stride = width * 4;

    // Frame 0: full red.
    const frame0 = new Uint8Array(stride * height);
    for (let i = 0; i < frame0.length; i += 4) {
      frame0[i + 0] = 255;
      frame0[i + 1] = 0;
      frame0[i + 2] = 0;
      frame0[i + 3] = 255;
    }
    wasm.upload_rgba8888(frame0, stride);

    // Frame 1: full blue, with a green dirty rect.
    const frame1 = new Uint8Array(stride * height);
    for (let i = 0; i < frame1.length; i += 4) {
      frame1[i + 0] = 0;
      frame1[i + 1] = 0;
      frame1[i + 2] = 255;
      frame1[i + 3] = 255;
    }

    const rect = { x: 4, y: 5, w: 3, h: 2 };
    for (let y = rect.y; y < rect.y + rect.h; y += 1) {
      for (let x = rect.x; x < rect.x + rect.w; x += 1) {
        const off = y * stride + x * 4;
        frame1[off + 0] = 0;
        frame1[off + 1] = 255;
        frame1[off + 2] = 0;
        frame1[off + 3] = 255;
      }
    }

    wasm.upload_rgba8888_dirty_rects(frame1, stride, new Uint32Array([rect.x, rect.y, rect.w, rect.h]));

    // Frame 2: full yellow, with a single-row cyan dirty rect. This exercises the
    // `copyHeight == 1` upload path.
    const frame2 = new Uint8Array(stride * height);
    for (let i = 0; i < frame2.length; i += 4) {
      frame2[i + 0] = 255;
      frame2[i + 1] = 255;
      frame2[i + 2] = 0;
      frame2[i + 3] = 255;
    }
    const rowRect = { x: 1, y: 2, w: 4, h: 1 };
    for (let y = rowRect.y; y < rowRect.y + rowRect.h; y += 1) {
      for (let x = rowRect.x; x < rowRect.x + rowRect.w; x += 1) {
        const off = y * stride + x * 4;
        frame2[off + 0] = 0;
        frame2[off + 1] = 255;
        frame2[off + 2] = 255;
        frame2[off + 3] = 255;
      }
    }
    wasm.upload_rgba8888_dirty_rects(frame2, stride, new Uint32Array([rowRect.x, rowRect.y, rowRect.w, rowRect.h]));

    const afterStats = (wasm.get_gpu_stats?.() as any) ?? {};

    const rgba = await wasm.request_screenshot();
    const rgbaBase64 = uint8ToBase64(rgba);

    try {
      wasm.destroy_gpu();
    } catch {
      // Ignore.
    }

    return {
      backend,
      width,
      height,
      rgbaBase64,
      statsDelta: {
        fullBytes: (afterStats.rgba8_upload_bytes_full ?? 0) - (beforeStats.rgba8_upload_bytes_full ?? 0),
        dirtyBytes:
          (afterStats.rgba8_upload_bytes_dirty_rects ?? 0) -
          (beforeStats.rgba8_upload_bytes_dirty_rects ?? 0),
        fullCalls: (afterStats.rgba8_upload_full_calls ?? 0) - (beforeStats.rgba8_upload_full_calls ?? 0),
        dirtyCalls: (afterStats.rgba8_upload_dirty_calls ?? 0) - (beforeStats.rgba8_upload_dirty_calls ?? 0),
        dirtyRects: (afterStats.rgba8_upload_dirty_rects ?? 0) - (beforeStats.rgba8_upload_dirty_rects ?? 0),
      },
    };
  });

  expect(result.backend).toBe('webgl2');
  expect(result.width).toBe(16);
  expect(result.height).toBe(16);
  expect(result.statsDelta.fullCalls).toBe(1);
  expect(result.statsDelta.dirtyCalls).toBe(2);
  expect(result.statsDelta.dirtyRects).toBe(2);
  expect(result.statsDelta.fullBytes).toBe(16 * 16 * 4);
  expect(result.statsDelta.dirtyBytes).toBe(3 * 2 * 4 + 4 * 1 * 4);

  const actual = Buffer.from(result.rgbaBase64, 'base64');
  expect(actual.byteLength).toBe(16 * 16 * 4);

  const expected = Buffer.alloc(16 * 16 * 4);
  const rect = { x: 4, y: 5, w: 3, h: 2 };
  const rowRect = { x: 1, y: 2, w: 4, h: 1 };
  for (let y = 0; y < 16; y += 1) {
    for (let x = 0; x < 16; x += 1) {
      const off = (y * 16 + x) * 4;
      const inRect = x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h;
      const inRowRect =
        x >= rowRect.x && x < rowRect.x + rowRect.w && y >= rowRect.y && y < rowRect.y + rowRect.h;
      if (inRect) {
        // Green.
        expected[off + 0] = 0;
        expected[off + 1] = 255;
        expected[off + 2] = 0;
        expected[off + 3] = 255;
      } else if (inRowRect) {
        // Cyan.
        expected[off + 0] = 0;
        expected[off + 1] = 255;
        expected[off + 2] = 255;
        expected[off + 3] = 255;
      } else {
        // Red (from the previous full-frame upload).
        expected[off + 0] = 255;
        expected[off + 1] = 0;
        expected[off + 2] = 0;
        expected[off + 3] = 255;
      }
    }
  }

  expect(sha256Hex(actual)).toBe(sha256Hex(expected));
  expect(actual.equals(expected)).toBe(true);
});

test('aero-gpu-wasm upload_rgba8888_dirty_rects falls back to full upload when rects are invalid', async ({ page }, testInfo) => {
  // The wgpu WebGL2 backend used by `aero-gpu-wasm` can be flaky in some browsers/headless
  // configurations; keep this test on the default Chromium project.
  test.skip(testInfo.project.name !== 'chromium', 'wgpu WebGL2 upload test only runs in Chromium');

  await page.goto('/web/src/pages/blank.html', { waitUntil: 'load' });

  const caps = await page.evaluate(() => {
    return {
      offscreen: typeof OffscreenCanvas !== 'undefined',
      transfer:
        typeof HTMLCanvasElement !== 'undefined' &&
        typeof (HTMLCanvasElement.prototype as any).transferControlToOffscreen === 'function',
    };
  });
  test.skip(!caps.offscreen, 'OffscreenCanvas is unavailable in this browser');
  test.skip(!caps.transfer, 'HTMLCanvasElement.transferControlToOffscreen() is unavailable in this browser');

  const result = await page.evaluate(async () => {
    function uint8ToBase64(u8: Uint8Array): string {
      const chunkSize = 0x8000;
      let binary = '';
      for (let i = 0; i < u8.length; i += chunkSize) {
        const chunk = u8.subarray(i, i + chunkSize);
        binary += String.fromCharCode.apply(null, Array.from(chunk));
      }
      return btoa(binary);
    }

    const wasm = await import('/web/src/wasm/aero-gpu.ts');
    await wasm.default();

    const beforeStats = (wasm.get_gpu_stats?.() as any) ?? {};

    const width = 16;
    const height = 16;
    const htmlCanvas = document.createElement('canvas');
    htmlCanvas.width = width;
    htmlCanvas.height = height;
    document.body.appendChild(htmlCanvas);
    const canvas = htmlCanvas.transferControlToOffscreen();

    try {
      wasm.destroy_gpu();
    } catch {
      // Ignore.
    }

    await wasm.init_gpu(canvas, width, height, 1, {
      preferWebGpu: false,
      disableWebGpu: true,
    });

    const backend = wasm.backend_kind();
    const stride = width * 4;

    // Frame 0: full red.
    const frame0 = new Uint8Array(stride * height);
    for (let i = 0; i < frame0.length; i += 4) {
      frame0[i + 0] = 255;
      frame0[i + 1] = 0;
      frame0[i + 2] = 0;
      frame0[i + 3] = 255;
    }
    wasm.upload_rgba8888(frame0, stride);

    // Frame 1: full blue. Provide an invalid dirty rect so the wasm code should fall back to
    // a full-frame upload (defensive behavior).
    const frame1 = new Uint8Array(stride * height);
    for (let i = 0; i < frame1.length; i += 4) {
      frame1[i + 0] = 0;
      frame1[i + 1] = 0;
      frame1[i + 2] = 255;
      frame1[i + 3] = 255;
    }
    // Completely out of bounds.
    wasm.upload_rgba8888_dirty_rects(frame1, stride, new Uint32Array([999, 999, 10, 10]));

    const afterStats = (wasm.get_gpu_stats?.() as any) ?? {};

    const rgba = await wasm.request_screenshot();
    const rgbaBase64 = uint8ToBase64(rgba);

    try {
      wasm.destroy_gpu();
    } catch {
      // Ignore.
    }

    return {
      backend,
      width,
      height,
      rgbaBase64,
      statsDelta: {
        fullBytes: (afterStats.rgba8_upload_bytes_full ?? 0) - (beforeStats.rgba8_upload_bytes_full ?? 0),
        dirtyBytes:
          (afterStats.rgba8_upload_bytes_dirty_rects ?? 0) -
          (beforeStats.rgba8_upload_bytes_dirty_rects ?? 0),
        fullCalls: (afterStats.rgba8_upload_full_calls ?? 0) - (beforeStats.rgba8_upload_full_calls ?? 0),
        dirtyCalls: (afterStats.rgba8_upload_dirty_calls ?? 0) - (beforeStats.rgba8_upload_dirty_calls ?? 0),
        dirtyRects: (afterStats.rgba8_upload_dirty_rects ?? 0) - (beforeStats.rgba8_upload_dirty_rects ?? 0),
      },
    };
  });

  expect(result.backend).toBe('webgl2');
  expect(result.width).toBe(16);
  expect(result.height).toBe(16);
  // One initial full upload (red) + one full upload fallback (blue).
  expect(result.statsDelta.fullCalls).toBe(2);
  expect(result.statsDelta.fullBytes).toBe(2 * 16 * 16 * 4);
  // The dirty-rect API was invoked but should upload no rect bytes (all invalid) and fall back.
  expect(result.statsDelta.dirtyCalls).toBe(1);
  expect(result.statsDelta.dirtyRects).toBe(0);
  expect(result.statsDelta.dirtyBytes).toBe(0);

  const actual = Buffer.from(result.rgbaBase64, 'base64');
  expect(actual.byteLength).toBe(16 * 16 * 4);

  const expected = Buffer.alloc(16 * 16 * 4);
  for (let i = 0; i < expected.length; i += 4) {
    expected[i + 0] = 0;
    expected[i + 1] = 0;
    expected[i + 2] = 255;
    expected[i + 3] = 255;
  }

  expect(sha256Hex(actual)).toBe(sha256Hex(expected));
  expect(actual.equals(expected)).toBe(true);
});
