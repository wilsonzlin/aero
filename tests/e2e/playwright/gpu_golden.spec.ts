import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { test, type Page } from '@playwright/test';

import { isWebGPURequired } from '../util/env';
import { expectRgbaToMatchGolden, type RgbaImage } from './utils/image_diff';

const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));

const GOLDEN_VGA_TEXT_MODE = 'vga_text_mode';
const GOLDEN_VBE_LFB_COLOR_BARS = 'vbe_lfb_color_bars_320x200';
const GOLDEN_WEBGL2_QUADRANTS = 'webgl2_quadrants_64';
const GOLDEN_WEBGPU_QUADRANTS = 'webgpu_quadrants_64';
const GOLDEN_GPU_SMOKE_QUADRANTS = 'gpu_smoke_quadrants_64';
const GOLDEN_GPU_TRACE_TRIANGLE_RED = 'gpu_trace_triangle_red_64';
const GOLDEN_GPU_TRACE_AEROGPU_A3A0_CLEAR_RED = 'gpu_trace_aerogpu_a3a0_clear_red_64';
const GOLDEN_GPU_TRACE_AEROGPU_CMD_TRIANGLE = 'gpu_trace_aerogpu_cmd_triangle_64';

function base64ToBuffer(base64: string): Buffer {
  return Buffer.from(base64, 'base64');
}

async function skipIfWebGl2Unavailable(page: Page, projectName: string): Promise<void> {
  const hasWebgl2 = await page.evaluate(() => {
    const canvas = document.createElement('canvas');
    return !!canvas.getContext('webgl2');
  });
  if (!hasWebgl2) {
    // Chromium is expected to have WebGL2 available (we rely on SwiftShader in CI for determinism).
    // Firefox headless environments may not provide WebGL2/WebGL at all; treat that as a skip so
    // the rest of the GPU golden suite (Canvas2D + Chromium WebGL2) can still run.
    if (projectName.startsWith('chromium')) {
      throw new Error('WebGL2 is unavailable in this Chromium test environment');
    }
    test.skip(true, 'WebGL2 is unavailable in this browser/environment');
  }
}

function resolveGoldenPngPath(goldenName: string): string {
  return path.resolve(TEST_DIR, '..', '..', 'golden', `${goldenName}.png`);
}

function tryReadPngSize(filePath: string): { width: number; height: number } | null {
  if (!fs.existsSync(filePath)) return null;
  const buf = fs.readFileSync(filePath);
  // PNG signature: 89 50 4E 47 0D 0A 1A 0A
  if (buf.length < 24 || buf[0] !== 0x89 || buf.toString('ascii', 1, 4) !== 'PNG') {
    throw new Error(`Invalid PNG file: ${filePath}`);
  }
  // IHDR starts at offset 8 (sig) + 4 (len) + 4 (type). Width/height are big-endian.
  const width = buf.readUInt32BE(16);
  const height = buf.readUInt32BE(20);
  return { width, height };
}

function readTraceHeader(tracePath: string): { containerVersion: number; commandAbiVersion: number } {
  const fd = fs.openSync(tracePath, 'r');
  try {
    const buf = Buffer.alloc(32);
    const n = fs.readSync(fd, buf, 0, 32, 0);
    if (n !== 32) throw new Error(`Short read for trace header: ${tracePath} (${n}/32 bytes)`);
    if (buf.toString('ascii', 0, 8) !== 'AEROGPUT') {
      throw new Error(`Invalid trace magic: ${tracePath}`);
    }
    const headerSize = buf.readUInt32LE(8);
    if (headerSize !== 32) throw new Error(`Unsupported trace header_size=${headerSize}: ${tracePath}`);
    const containerVersion = buf.readUInt32LE(12);
    const commandAbiVersion = buf.readUInt32LE(16);
    return { containerVersion, commandAbiVersion };
  } finally {
    fs.closeSync(fd);
  }
}

function browserUint8ToBase64Source(): string {
  // Chunked btoa to avoid call stack limits.
  return `
    function __aeroUint8ToBase64(u8) {
      const chunkSize = 0x8000;
      let binary = '';
      for (let i = 0; i < u8.length; i += chunkSize) {
        const chunk = u8.subarray(i, i + chunkSize);
        binary += String.fromCharCode.apply(null, chunk);
      }
      return btoa(binary);
    }
  `;
}

async function captureCanvas2dRGBA(page: Page, selector: string): Promise<RgbaImage> {
  const result = await page.evaluate(
    ({ selector }: { selector: string }) => {
      const canvas = document.querySelector(selector);
      if (!(canvas instanceof HTMLCanvasElement)) throw new Error(`Missing canvas: ${selector}`);
      const ctx = canvas.getContext('2d');
      if (!ctx) throw new Error('2d context unavailable');
      const { width, height } = canvas;
      const img = ctx.getImageData(0, 0, width, height);
      const rgbaBase64 = (window as any).__aeroUint8ToBase64(new Uint8Array(img.data.buffer));
      return { width, height, rgbaBase64 };
    },
    { selector }
  );

  return {
    width: result.width,
    height: result.height,
    rgba: base64ToBuffer(result.rgbaBase64)
  };
}

async function captureGpuSmokeFrameRGBA(page: Page): Promise<RgbaImage> {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
  const result = await page.evaluate(async () => {
    const api = (window as any).__aeroTest;
    if (api?.error) throw new Error(`gpu-smoke error: ${api.error}`);
    if (!api?.captureFrameBase64) throw new Error('__aeroTest.captureFrameBase64 missing');
    return await api.captureFrameBase64();
  });

  return {
    width: result.width,
    height: result.height,
    rgba: base64ToBuffer(result.rgbaBase64),
  };
}

test.beforeEach(async ({ page }) => {
  // `page.setContent()` uses `document.write()` (no navigation), so `addInitScript`
  // does not run. Inject the helper into the current document instead so it remains
  // available across `setContent()` calls.
  await page.addScriptTag({ content: browserUint8ToBase64Source() });
});

test('VGA text mode microtest (chars+attrs) matches golden', async ({ page }, testInfo) => {
  await page.setContent(`<canvas id="c"></canvas>`);
  await page.addScriptTag({ path: path.join(TEST_DIR, 'scenes/vga_text_mode_scene.cjs') });

  await page.evaluate(() => {
    const { width, height, rgba } = (window as any).AeroTestScenes.renderVgaTextModeSceneRGBA();
    const canvas = document.getElementById('c');
    if (!(canvas instanceof HTMLCanvasElement)) throw new Error('Missing canvas');
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('2d context unavailable');
    const imageData = new ImageData(new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength), width, height);
    ctx.putImageData(imageData, 0, 0);
  });

  const actual = await captureCanvas2dRGBA(page, '#c');
  await expectRgbaToMatchGolden(testInfo, GOLDEN_VGA_TEXT_MODE, actual, { maxDiffPixels: 0, threshold: 0 });
});

test('VBE LFB microtest (color bars) matches golden', async ({ page }, testInfo) => {
  await page.setContent(`<canvas id="c"></canvas>`);
  await page.addScriptTag({ path: path.join(TEST_DIR, 'scenes/vbe_lfb_scene.cjs') });

  await page.evaluate(() => {
    const { width, height, rgba } = (window as any).AeroTestScenes.renderVbeLfbColorBarsRGBA();
    const canvas = document.getElementById('c');
    if (!(canvas instanceof HTMLCanvasElement)) throw new Error('Missing canvas');
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('2d context unavailable');
    const imageData = new ImageData(new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength), width, height);
    ctx.putImageData(imageData, 0, 0);
  });

  const actual = await captureCanvas2dRGBA(page, '#c');
  await expectRgbaToMatchGolden(testInfo, GOLDEN_VBE_LFB_COLOR_BARS, actual, { maxDiffPixels: 0, threshold: 0 });
});

test('WebGL2 microtest (scissored clears) matches golden', async ({ page }, testInfo) => {
  await page.setContent(`<canvas id="c" width="64" height="64"></canvas>`);
  await skipIfWebGl2Unavailable(page, testInfo.project.name);

  const result = await page.evaluate(() => {
    const canvas = document.getElementById('c');
    if (!(canvas instanceof HTMLCanvasElement)) throw new Error('Missing canvas');

    const gl = canvas.getContext('webgl2', { preserveDrawingBuffer: true });
    if (!gl) throw new Error('WebGL2 unavailable');

    const w = canvas.width;
    const h = canvas.height;
    const midX = Math.floor(w / 2);
    const midY = Math.floor(h / 2);

    gl.disable(gl.DITHER);
    gl.disable(gl.BLEND);
    gl.viewport(0, 0, w, h);
    gl.enable(gl.SCISSOR_TEST);

    // Clear each quadrant with integer scissor bounds.
    gl.scissor(0, h - midY, midX, midY); // top-left (note y=0 is bottom)
    gl.clearColor(1, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.scissor(midX, h - midY, w - midX, midY); // top-right
    gl.clearColor(0, 1, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.scissor(0, 0, midX, h - midY); // bottom-left
    gl.clearColor(0, 0, 1, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.scissor(midX, 0, w - midX, h - midY); // bottom-right
    gl.clearColor(1, 1, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.finish();

    const pixels = new Uint8Array(w * h * 4);
    gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

    // WebGL's origin is bottom-left; flip vertically into a new buffer.
    const flipped = new Uint8Array(w * h * 4);
    const rowSize = w * 4;
    for (let y = 0; y < h; y++) {
      const srcStart = y * rowSize;
      const dstStart = (h - 1 - y) * rowSize;
      flipped.set(pixels.subarray(srcStart, srcStart + rowSize), dstStart);
    }

    // eslint-disable-next-line no-undef
    const rgbaBase64 = (window as any).__aeroUint8ToBase64(flipped);
    return { width: w, height: h, rgbaBase64 };
  });

  const actual: RgbaImage = {
    width: result.width,
    height: result.height,
    rgba: base64ToBuffer(result.rgbaBase64)
  };

  await expectRgbaToMatchGolden(testInfo, GOLDEN_WEBGL2_QUADRANTS, actual, { maxDiffPixels: 0, threshold: 0 });
});

test('WebGPU microtest (scissored quad) matches golden @webgpu', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-webgpu', 'WebGPU is only enabled in the Chromium project.');

  await page.setContent(`<canvas id="c" width="64" height="64"></canvas>`);

  const hasNavigatorGpu = await page.evaluate(() => !!(navigator as any).gpu);
  if (!hasNavigatorGpu) {
    if (isWebGPURequired()) {
      throw new Error('WebGPU is unavailable: `navigator.gpu` is missing');
    }
    test.skip(true, 'WebGPU is unavailable: `navigator.gpu` is missing');
  }

  let result: { width: number; height: number; rgbaBase64: string };
  try {
    result = await page.evaluate(async () => {
      const canvas = document.getElementById('c');
      if (!(canvas instanceof HTMLCanvasElement)) throw new Error('Missing canvas');
      const gpu = (navigator as any).gpu as GPU | undefined;
      if (!gpu) throw new Error('navigator.gpu unavailable');

      const adapter = await gpu.requestAdapter();
      if (!adapter) throw new Error('WebGPU adapter unavailable');
      const device = await adapter.requestDevice();
      const format = gpu.getPreferredCanvasFormat();
      const isBGRA = String(format).startsWith('bgra');

      const context = canvas.getContext('webgpu') as unknown as GPUCanvasContext | null;
      if (!context) throw new Error('webgpu context unavailable');

      context.configure({
        device,
        format,
        alphaMode: 'opaque',
        usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
      });

      const shader = device.createShaderModule({
        code: `
          struct Uniforms { color: vec4<f32> };
          @group(0) @binding(0) var<uniform> u: Uniforms;

          @vertex fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
            var pos = array<vec2<f32>, 3>(
              vec2<f32>(-1.0, -1.0),
              vec2<f32>( 3.0, -1.0),
              vec2<f32>(-1.0,  3.0)
            );
            return vec4<f32>(pos[vi], 0.0, 1.0);
          }

          @fragment fn fs() -> @location(0) vec4<f32> {
            return u.color;
          }
        `,
      });

      const pipeline = device.createRenderPipeline({
        layout: 'auto',
        vertex: { module: shader, entryPoint: 'vs' },
        fragment: { module: shader, entryPoint: 'fs', targets: [{ format }] },
        primitive: { topology: 'triangle-list' },
      });

      const uniformBuffer = device.createBuffer({
        size: 16,
        usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
      });

      const bindGroup = device.createBindGroup({
        layout: pipeline.getBindGroupLayout(0),
        entries: [{ binding: 0, resource: { buffer: uniformBuffer } }],
      });

      const w = canvas.width;
      const h = canvas.height;
      const midX = Math.floor(w / 2);
      const midY = Math.floor(h / 2);

      const texture = context.getCurrentTexture();
      const encoder = device.createCommandEncoder();
      const pass = encoder.beginRenderPass({
        colorAttachments: [
          {
            view: texture.createView(),
            clearValue: { r: 0, g: 0, b: 0, a: 1 },
            loadOp: 'clear',
            storeOp: 'store',
          },
        ],
      });

      pass.setPipeline(pipeline);
      pass.setBindGroup(0, bindGroup);

      const drawScissored = (
        x: number,
        y: number,
        sw: number,
        sh: number,
        rgba: [number, number, number, number]
      ) => {
        device.queue.writeBuffer(uniformBuffer, 0, new Float32Array(rgba));
        pass.setScissorRect(x, y, sw, sh);
        pass.draw(3, 1, 0, 0);
      };

      // y=0 is top in WebGPU scissor coords.
      drawScissored(0, 0, midX, midY, [1, 0, 0, 1]); // top-left
      drawScissored(midX, 0, w - midX, midY, [0, 1, 0, 1]); // top-right
      drawScissored(0, midY, midX, h - midY, [0, 0, 1, 1]); // bottom-left
      drawScissored(midX, midY, w - midX, h - midY, [1, 1, 0, 1]); // bottom-right

      pass.end();

      const bytesPerPixel = 4;
      const unpaddedBytesPerRow = w * bytesPerPixel;
      const align = (n: number, a: number) => Math.ceil(n / a) * a;
      const bytesPerRow = align(unpaddedBytesPerRow, 256);

      const readback = device.createBuffer({
        size: bytesPerRow * h,
        usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
      });

      encoder.copyTextureToBuffer({ texture }, { buffer: readback, bytesPerRow }, { width: w, height: h, depthOrArrayLayers: 1 });

      device.queue.submit([encoder.finish()]);

      await readback.mapAsync(GPUMapMode.READ);
      const mapped = new Uint8Array(readback.getMappedRange());

      // Convert padded BGRA rows -> tightly packed RGBA.
      const rgba = new Uint8Array(w * h * 4);
      for (let y = 0; y < h; y++) {
        const srcRow = y * bytesPerRow;
        const dstRow = y * unpaddedBytesPerRow;
        for (let x = 0; x < w; x++) {
          const si = srcRow + x * 4;
          const di = dstRow + x * 4;
          const c0 = mapped[si + 0];
          const c1 = mapped[si + 1];
          const c2 = mapped[si + 2];
          const c3 = mapped[si + 3];
          if (isBGRA) {
            rgba[di + 0] = c2;
            rgba[di + 1] = c1;
            rgba[di + 2] = c0;
            rgba[di + 3] = c3;
          } else {
            rgba[di + 0] = c0;
            rgba[di + 1] = c1;
            rgba[di + 2] = c2;
            rgba[di + 3] = c3;
          }
        }
      }

      readback.unmap();
      const rgbaBase64 = (window as any).__aeroUint8ToBase64(rgba);
      return { width: w, height: h, rgbaBase64 };
    });
  } catch (error) {
    if (isWebGPURequired()) {
      throw error;
    }
    test.skip(true, `WebGPU not usable in this environment: ${String(error)}`);
  }

  const actual: RgbaImage = {
    width: result.width,
    height: result.height,
    rgba: base64ToBuffer(result.rgbaBase64)
  };

  await expectRgbaToMatchGolden(testInfo, GOLDEN_WEBGPU_QUADRANTS, actual, { maxDiffPixels: 0, threshold: 0 });
});

test('GPU backend smoke: WebGL2 presents expected frame (golden)', async ({ page }, testInfo) => {
  await skipIfWebGl2Unavailable(page, testInfo.project.name);
  await page.goto('/web/gpu-smoke.html?backend=webgl2&filter=nearest&aspect=stretch', {
    waitUntil: 'load',
  });
  const actual = await captureGpuSmokeFrameRGBA(page);
  await expectRgbaToMatchGolden(testInfo, GOLDEN_GPU_SMOKE_QUADRANTS, actual, { maxDiffPixels: 0, threshold: 0 });
});

test('GPU backend smoke: WebGPU presents expected frame (golden) @webgpu', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-webgpu', 'WebGPU smoke only runs on Chromium WebGPU project.');

  try {
    await page.goto('/web/gpu-smoke.html?backend=webgpu&filter=nearest&aspect=stretch', {
      waitUntil: 'load',
    });
    const hasNavigatorGpu = await page.evaluate(() => !!(navigator as any).gpu);
    if (!hasNavigatorGpu) {
      if (isWebGPURequired()) {
        throw new Error('WebGPU is unavailable: `navigator.gpu` is missing');
      }
      test.skip(true, 'WebGPU is unavailable: `navigator.gpu` is missing');
    }
    const actual = await captureGpuSmokeFrameRGBA(page);
    await expectRgbaToMatchGolden(testInfo, GOLDEN_GPU_SMOKE_QUADRANTS, actual, { maxDiffPixels: 0, threshold: 0 });
  } catch (error) {
    if (isWebGPURequired()) {
      throw error;
    }
    test.skip(true, `WebGPU not usable in this environment: ${String(error)}`);
  }
});

async function captureGpuTraceReplayFrameRGBA(
  page: Page,
  args: {
    traceB64: string;
    frameIndex?: number;
    backend?: 'webgl2' | 'webgpu';
  }
): Promise<RgbaImage> {
  const result = await page.evaluate(
    async ({ traceB64, frameIndex, backend }: { traceB64: string; frameIndex?: number; backend?: 'webgl2' | 'webgpu' }) => {
      const raw = atob(traceB64);
      const bytes = new Uint8Array(raw.length);
      for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);

      const canvas = document.getElementById('c');
      if (!(canvas instanceof HTMLCanvasElement)) throw new Error('missing canvas');

      const backendName = backend ?? 'webgl2';
      const replayer = await (window as any).AeroGpuTraceReplay.load(bytes, canvas, { backend: backendName });
      await replayer.replayFrame(frameIndex ?? 0);

      const w = canvas.width;
      const h = canvas.height;
      const pixels = replayer.readPixels();

      // WebGL origin is bottom-left; WebGPU origin is top-left.
      let out = pixels;
      if (backendName === 'webgl2') {
        const flipped = new Uint8Array(w * h * 4);
        const rowSize = w * 4;
        for (let y = 0; y < h; y++) {
          const srcStart = y * rowSize;
          const dstStart = (h - 1 - y) * rowSize;
          flipped.set(pixels.subarray(srcStart, srcStart + rowSize), dstStart);
        }
        out = flipped;
      }

      const rgbaBase64 = (window as any).__aeroUint8ToBase64(out);
      return { width: w, height: h, rgbaBase64 };
    },
    args
  );

  return {
    width: result.width,
    height: result.height,
    rgba: base64ToBuffer(result.rgbaBase64),
  };
}

const TRACE_FIXTURE_DIR = path.resolve(TEST_DIR, '../../fixtures');
const TRACE_TOOL_PATH = path.resolve(TEST_DIR, '../../../web/tools/gpu_trace_replay.ts');

// Map fixture filename -> golden override.
//
// Note: Only trace fixtures that have a corresponding committed PNG golden under
// `tests/golden/` are included in the browser golden suite. Additional trace
// fixtures may exist purely for Rust-side replay/hash tests (e.g. formats not yet
// supported by the JS/WebGL trace backend), and those should not fail browser CI.
const TRACE_GOLDEN_OVERRIDES: Record<string, string> = {
  // Historical name (kept for compatibility with existing committed goldens).
  'triangle.aerogputrace': GOLDEN_GPU_TRACE_TRIANGLE_RED,
  // Explicitly listed for clarity.
  'aerogpu_a3a0_clear_red.aerogputrace': GOLDEN_GPU_TRACE_AEROGPU_A3A0_CLEAR_RED,
  'aerogpu_cmd_triangle.aerogputrace': GOLDEN_GPU_TRACE_AEROGPU_CMD_TRIANGLE,
};

for (const traceFile of fs
  .readdirSync(TRACE_FIXTURE_DIR)
  .filter((f) => f.endsWith('.aerogputrace'))
  .sort()) {
  const tracePath = path.resolve(TRACE_FIXTURE_DIR, traceFile);
  const traceHeader = readTraceHeader(tracePath);
  const goldenName =
    TRACE_GOLDEN_OVERRIDES[traceFile] ??
    `gpu_trace_${path.basename(traceFile, '.aerogputrace')}_64`;
  const goldenPath = resolveGoldenPngPath(goldenName);
  const goldenSize = tryReadPngSize(goldenPath);
  const canvasWidth = goldenSize?.width ?? 64;
  const canvasHeight = goldenSize?.height ?? 64;

  const describeFn = goldenSize ? test.describe : test.describe.skip;
  describeFn(`GPU trace replay: ${traceFile}`, () => {
    if (!goldenSize) {
      test.skip(true, `No committed golden found at ${goldenPath}; this fixture is not part of the browser golden suite.`);
    }

    test(`renders deterministically (golden)`, async ({ page }, testInfo) => {
      const traceB64 = fs.readFileSync(tracePath).toString('base64');

      await page.setContent(`<canvas id="c" width="${canvasWidth}" height="${canvasHeight}"></canvas>`);
      await skipIfWebGl2Unavailable(page, testInfo.project.name);
      await page.addScriptTag({ path: TRACE_TOOL_PATH });

      const actual = await captureGpuTraceReplayFrameRGBA(page, { traceB64, frameIndex: 0, backend: 'webgl2' });
      await expectRgbaToMatchGolden(testInfo, goldenName, actual, { maxDiffPixels: 0, threshold: 0 });
    });

    const webgpuTraceSupported =
      // Minimal reference command ABI v1.
      traceHeader.commandAbiVersion === 1 ||
      // AeroGPU command stream ABI v1 (0x0001_xxxx).
      (traceHeader.commandAbiVersion >>> 16) === 1;
    if (webgpuTraceSupported) {
      test(`renders deterministically (golden) @webgpu`, async ({ page }, testInfo) => {
        test.skip(testInfo.project.name !== 'chromium-webgpu', 'WebGPU trace replay only runs on Chromium WebGPU project.');

        const hasNavigatorGpu = await page.evaluate(() => !!(navigator as any).gpu);
        if (!hasNavigatorGpu) {
          if (isWebGPURequired()) {
            throw new Error('WebGPU is unavailable: `navigator.gpu` is missing');
          }
          test.skip(true, 'WebGPU is unavailable: `navigator.gpu` is missing');
        }

        try {
          const traceB64 = fs.readFileSync(tracePath).toString('base64');

          await page.setContent(`<canvas id="c" width="${canvasWidth}" height="${canvasHeight}"></canvas>`);
          await page.addScriptTag({ path: TRACE_TOOL_PATH });

          const actual = await captureGpuTraceReplayFrameRGBA(page, { traceB64, frameIndex: 0, backend: 'webgpu' });
          await expectRgbaToMatchGolden(testInfo, goldenName, actual, { maxDiffPixels: 0, threshold: 0 });
        } catch (error) {
          if (isWebGPURequired()) {
            throw error;
          }
          test.skip(true, `WebGPU not usable in this environment: ${String(error)}`);
        }
      });
    }
  });
}
